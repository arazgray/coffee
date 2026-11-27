"""Post Cocoa keyboard events to the unmodified release and check actual actions."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time


def wait(test, timeout=15):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        result = test()
        if result:
            return result
        time.sleep(.035)
    return None


class Mpv:
    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(8)
        self.socket.connect(str(path))
        self.pipe = self.socket.makefile("rwb")
        self.request = 0

    def run(self, *args):
        self.request += 1
        self.pipe.write((json.dumps({"command": args, "request_id": self.request}) + "\n").encode())
        self.pipe.flush()
        while line := self.pipe.readline():
            reply = json.loads(line)
            if reply.get("request_id") != self.request:
                continue
            if reply["error"] == "property unavailable":
                return None
            assert reply["error"] == "success", (args, reply)
            return reply.get("data")
        raise AssertionError(f"mpv disconnected during {args}")

    def get(self, prop):
        return self.run("get_property", prop)

    def set(self, prop, value):
        self.run("set_property", prop, value)


with tempfile.TemporaryDirectory(prefix="coffee-keys-", dir="/tmp") as temp:
    temp = Path(temp)
    exe, probe, real_libmpv = sys.argv[1:]
    socket_path, request, events = (temp / name for name in ("mpv.sock", "request", "events"))
    video = temp / "keys.mp4"
    subprocess.run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i", "color=s=480x270:r=24:d=100",
                    "-f", "lavfi", "-i", "sine=frequency=440:duration=100", "-c:v", "mpeg4",
                    "-c:a", "aac", "-shortest", str(video)], check=True)
    env = {**os.environ, "MPVBUNDLE": "true", "XDG_RUNTIME_DIR": str(temp), "COFFEE_LIBMPV": probe,
           "COFFEE_PROBE_PROVIDER": real_libmpv, "COFFEE_PROBE_SOCKET": str(socket_path),
           "COFFEE_PROBE_REQUEST": str(request), "COFFEE_PROBE_EVENTS": str(events)}
    log = (temp / "output").open("w")
    app = subprocess.Popen([exe, str(video)], env=env, stdout=log, stderr=subprocess.STDOUT)
    try:
        assert wait(lambda: socket_path.is_socket()), "No playback IPC socket"
        mpv = Mpv(socket_path)
        assert wait(lambda: (mpv.get("osd-dimensions") or {}).get("w", 0) > 0), "No native video window"
        assert wait(lambda: mpv.get("duration") and mpv.get("time-pos") is not None), "No playing fixture"
        print(f"Native Cocoa window; mpv {mpv.get('mpv-version')}; Coffee {subprocess.check_output([exe, '--version'], text=True).strip()}",
              flush=True)
        mpv.set("pause", True)

        def reset(prop, start):
            if prop == "time-pos":
                mpv.run("seek", start, "absolute+exact")
                assert wait(lambda: not mpv.get("seeking") and abs((mpv.get(prop) or -1) - start) < .2)
            else:
                mpv.set(prop, start)
                assert wait(lambda: mpv.get(prop) == start)

        cases = [
            ("SPACE", 49, 0x20, "pause", True, False),
            ("k", 40, ord("k"), "pause", True, False),
            ("RIGHT", 124, 0xF703, "time-pos", 18, 28),
            ("LEFT", 123, 0xF702, "time-pos", 18, 8),
            ("UP", 126, 0xF700, "volume", 35, 40),
            ("DOWN", 125, 0xF701, "volume", 35, 30),
            ("m", 46, ord("m"), "mute", False, True),
            ("f", 3, ord("f"), "fullscreen", False, True),
        ]
        rows = []
        for i, (name, keycode, cp, prop, start, expected) in enumerate(cases, 1):
            reset(prop, start)
            staging = request.with_suffix(".tmp")
            staging.write_text(f"{i} {keycode} {cp:x}")
            staging.replace(request)

            def delivered():
                if not events.exists():
                    return False
                got = [line.split() for line in events.read_text().splitlines() if line]
                return all([str(i), str(kind), str(keycode), "1", "1"] in got for kind in (10, 11))

            def changed():
                value = mpv.get(prop)
                return (value is not None and abs(value - expected) < .3
                        if prop == "time-pos" else value == expected)

            assert wait(delivered), f"{name}: key down/up did not reach the active Cocoa window"
            native = bool(wait(changed, timeout=1.5))
            reset(prop, start)
            mpv.run("keypress", name)
            control = bool(wait(changed, timeout=3))
            print(f"{name}: Cocoa={native}, libmpv control={control}", flush=True)
            rows.append((name, native, control))
            if prop == "pause":
                mpv.set("pause", True)
            elif prop == "fullscreen":
                mpv.set("fullscreen", False)

        if output := os.environ.get("GITHUB_STEP_SUMMARY"):
            with open(output, "a") as summary:
                summary.write("| Shortcut | Native AppKit | libmpv control |\n|---|---|---|\n")
                for name, native, control in rows:
                    summary.write(f"| {name} | {'Works' if native else 'No action'} | {'Works' if control else 'No action'} |\n")
        assert all(control for _, _, control in rows), "One or more backend control checks failed"
    finally:
        if app.poll() is None:
            app.terminate()
            app.wait(timeout=8)
        log.close()
        print((temp / "output").read_text(), flush=True)
