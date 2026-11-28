"""Exercise real AppKit key events on both macOS release runners."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time


def wait(test, message):
    end = time.monotonic() + 12
    while time.monotonic() < end:
        if test():
            return
        time.sleep(.04)
    raise AssertionError(message)


class Mpv:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(8)
        self.sock.connect(str(path))
        self.pipe = self.sock.makefile("rwb")

    def call(self, *args):
        self.pipe.write((json.dumps({"command": args, "request_id": 1}) + "\n").encode())
        self.pipe.flush()
        while line := self.pipe.readline():
            event = json.loads(line)
            if event.get("request_id") != 1:
                continue
            if event["error"] == "property unavailable":
                return None
            assert event["error"] == "success", (args, event)
            return event.get("data")
        raise AssertionError("Coffee disconnected")


with tempfile.TemporaryDirectory(prefix="coffee-keys-", dir="/tmp") as folder:
    root = Path(folder)
    app, shim, provider = sys.argv[1:]
    video, ipc, key, modal = (root / name for name in ("fixture.mp4", "ipc", "key", "modal"))
    subprocess.run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i", "color=s=320x180:r=24:d=60",
                    "-f", "lavfi", "-i", "sine=frequency=440:duration=60", "-c:v", "mpeg4",
                    "-g", "12", "-c:a", "aac", "-shortest", video], check=True)
    env = {**os.environ, "MPVBUNDLE": "true", "XDG_RUNTIME_DIR": folder, "COFFEE_LIBMPV": shim,
           "COFFEE_TEST_PROVIDER": provider, "COFFEE_TEST_IPC": str(ipc),
           "COFFEE_TEST_KEY": str(key), "COFFEE_TEST_MODAL": str(modal)}
    with (root / "player.log").open("w") as log:
        player = subprocess.Popen([app, video], env=env, stdout=log, stderr=subprocess.STDOUT)
        try:
            wait(ipc.is_socket, "Coffee did not start")
            mpv = Mpv(ipc)
            get = lambda prop: mpv.call("get_property", prop)
            set_ = lambda prop, value: mpv.call("set_property", prop, value)
            wait(lambda: get("time-pos") is not None, "Fixture did not play")
            set_("pause", True)

            def press(n, code, char, command=False):
                pending = key.with_suffix(".tmp")
                pending.write_text(f"{n} {code} {ord(char):x} {int(command)}")
                pending.replace(key)

            def near(value):
                return abs((get("time-pos") or 0) - value) < .3

            for n, (label, code, char, prop, before, after) in enumerate([
                ("Space", 49, " ", "pause", True, False),
                ("K", 40, "k", "pause", True, False),
                ("Right", 124, "\uf703", "time-pos", 18, 28),
                ("Left", 123, "\uf702", "time-pos", 18, 8),
                ("Up", 126, "\uf700", "volume", 35, 40),
                ("Down", 125, "\uf701", "volume", 35, 30),
                ("M", 46, "m", "mute", False, True),
                ("F", 3, "f", "fullscreen", False, True),
                ("Return", 36, "\r", "fullscreen", True, False),
            ], 1):
                if prop == "time-pos":
                    mpv.call("seek", before, "absolute+exact")
                    wait(lambda: not get("seeking") and near(before), "Fixture seek failed")
                else:
                    set_(prop, before)
                press(n, code, char)
                wait(lambda: near(after) if prop == "time-pos" else get(prop) == after,
                     f"{label} did not change {prop}")
                print(f"{label} triggered {prop}={after}", flush=True)
                if prop == "pause":
                    set_("pause", True)

            press(10, 31, "o", command=True)
            wait(modal.is_file, "Command+O did not open the macOS file panel")
            print("Command+O opened the macOS file panel", flush=True)
        finally:
            if player.poll() is None:
                player.terminate()
                player.wait(timeout=8)
            print((root / "player.log").read_text(), flush=True)
