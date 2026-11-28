"""Run the release app against a libmpv probe with one IPC socket per window."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time


def wait(check, message, timeout=12):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        result = check()
        if result:
            return result
        time.sleep(.04)
    raise AssertionError(message)


class Mpv:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(3)
        try:
            self.sock.connect(str(path))
        except OSError:
            self.sock.close()
            raise
        self.pipe = self.sock.makefile("rwb")

    def call(self, *args):
        self.pipe.write((json.dumps({"command": args, "request_id": 1}) + "\n").encode())
        self.pipe.flush()
        while line := self.pipe.readline():
            event = json.loads(line)
            if event.get("request_id") != 1:
                continue
            error = event["error"]
            if error == "property unavailable":
                return None
            assert error == "success", (args, event)
            return event.get("data")
        raise ConnectionError("window closed")

    def get(self, key):
        return self.call("get_property", key)

    def close(self):
        try:
            self.pipe.close()
        except OSError:
            pass
        finally:
            self.sock.close()


exe, shim, provider = map(str, sys.argv[1:])
mac = sys.platform == "darwin"
with tempfile.TemporaryDirectory(prefix="coffee-open-", dir="/tmp") as name:
    root = Path(name).resolve()
    sockets = root / "ipc"
    sockets.mkdir()
    a, song, b = (root / file for file in ("first.mp4", "song with spaces.wav", "next.mp4"))
    subprocess.run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i",
                    "color=s=320x180:r=24:d=30", "-c:v", "mpeg4", a], check=True)
    b.write_bytes(a.read_bytes())
    subprocess.run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i",
                    "sine=frequency=440:duration=30", song], check=True)
    red, blue = root / "first photo.png", root / "second photo.png"
    for photo, color in ((red, "red"), (blue, "blue")):
        subprocess.run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i", f"color={color}:s=32x24",
                        "-threads", "1", "-frames:v", "1", photo], check=True)
    overrides = {"MPVBUNDLE": "true", "XDG_RUNTIME_DIR": name,
                 "COFFEE_LIBMPV": shim, "COFFEE_TEST_PROVIDER": provider,
                 "COFFEE_TEST_IPC_DIR": str(sockets)}
    env = {**os.environ, **overrides}
    children, peers = [], {}
    logfile = root / "player.log"
    with logfile.open("wb") as log:
        def call(*files, finder=False, new_instance=False):
            if finder:
                bundle = Path(exe).parents[2]
                command = ["open", "-a", str(bundle)]
                if new_instance:
                    command.append("-n")
                for key, value in overrides.items():
                    command += ["--env", f"{key}={value}"]
                subprocess.run([*command, *files], env=env, check=True)
                return None
            child = subprocess.Popen([exe, *files], env=env, stdout=log, stderr=subprocess.STDOUT)
            children.append(child)
            return child

        def windows():
            found = {}
            for path in sorted(sockets.glob("*.sock")):
                try:
                    if path not in peers:
                        peers[path] = Mpv(path)
                    value = peers[path].get("path")
                    if value:
                        found[path] = value
                except OSError:
                    if path in peers:
                        peers.pop(path).close()
            return found

        def window_for(file):
            return next((key for key, path in windows().items() if path == str(file)), None)

        def filenames(player):
            return [entry["filename"] for entry in player.get("playlist")]

        try:
            # Cold photo open should not even claim the playback lock.
            call(red, finder=mac, new_instance=True)
            photo = wait(lambda: window_for(red), "Opening a photo did not create a viewer")
            assert not (root / "coffee" / "playback.lock").exists(), "Photo became the playback owner"
            wait(lambda: filenames(peers[photo]) == [str(red), str(blue)],
                 "Photo sibling navigation was lost")

            player = call(a)
            key = wait(lambda: window_for(a), "First video did not open")
            assert key.stem == str(player.pid)
            main = peers[key]
            second = call(song, finder=mac)
            wait(lambda: main.get("path") == str(song), "Opening a song did not switch the existing player")
            if second:
                assert second.wait(timeout=8) == 0
            assert filenames(main) == [str(a), str(song)]

            # Opening a photo while the song plays is independent, even
            # when the original photo window is still visible.
            call(blue, finder=mac)
            newest = wait(lambda: window_for(blue), "Second photo did not open separately")
            assert len({key, photo, newest}) == 3
            assert main.get("path") == str(song) and not main.get("pause")
            assert filenames(main) == [str(a), str(song)]

            call("--add-to-queue", b).wait(timeout=8)
            wait(lambda: filenames(main) == [str(a), str(song), str(b)], "Add files did not append")
            assert main.get("path") == str(song)
            call(red, b).wait(timeout=8)
            wait(lambda: (main.get("path") == str(b)
                          and list(windows().values()).count(str(red)) == 2),
                 "Mixed opens did not split photos from playback")
            assert filenames(main) == [str(a), str(song), str(b), str(b)]

            # Exercise the same append notification that libmpv uses for
            # native drag and drop.
            main.call("loadfile", str(blue), "append")
            wait(lambda: list(windows().values()).count(str(blue)) == 2,
                 "A dropped photo stayed in the player instead of opening separately")
            assert filenames(main) == [str(a), str(song), str(b), str(b)]
            try:
                peers[newest].call("quit")
            except ConnectionError:
                pass
            wait(lambda: newest not in windows(), "Photo viewer did not close")
            assert main.get("path") == str(b) and not main.get("pause")
            print("One video/music queue, independent photos, mixed opens, drops and queue-only adds verified.")
        finally:
            windows()
            for peer in list(peers.values()):
                try:
                    peer.call("quit")
                except OSError:
                    pass
                peer.close()
            for child in children:
                if child.poll() is None:
                    child.terminate()
                child.wait(timeout=8)
            print(logfile.read_text(errors="replace"), flush=True)
