#!/usr/bin/env python3
"""End-to-end test using Unix-socket stand-ins for a Hyprland session."""
import csv
from datetime import date, timedelta
import io
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request


def u32(*values):
    return struct.pack(f"={len(values)}I", *values)


def wire_string(value):
    data = value.encode() + b"\0"
    return u32(len(data)) + data + bytes(-len(data) % 4)


def read_exact(sock, n):
    out = bytearray()
    while len(out) != n:
        data = sock.recv(n - len(out))
        if not data:
            raise EOFError
        out.extend(data)
    return bytes(out)


class Compositor:
    globals = {10: ("wl_seat", 7), 11: ("ext_idle_notifier_v1", 2),
               12: ("hyprland_lock_notifier_v1", 1), 13: ("zwlr_output_power_manager_v1", 1),
               14: ("wl_output", 4), 15: ("wl_output", 4)}

    def __init__(self, root, native=True):
        self.globals = dict(self.globals)
        if not native:
            del self.globals[12]
            del self.globals[13]
        self.focus = self.wayland = None
        self.idle_id = self.lock_id = None
        self.power_ids = []
        self.use_input_idle = None
        self.command_log = []
        self.power_on = True
        self.locked = False
        self._errors = []
        self._wayland_write = threading.Lock()
        self.dir = root / "hypr" / "local_test"
        self.dir.mkdir(parents=True)
        self.listen(self.dir / ".socket2.sock", "focus", self.accept_focus)
        self.listen(self.dir / ".socket.sock", "command", self.serve_commands)
        self.listen(root / "wayland-test", "wayland", self.serve_wayland)

    def listen(self, path, name, handler):
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(str(path))
        server.listen(5)

        def run():
            try:
                with server:
                    handler(server)
            except (EOFError, ConnectionResetError, BrokenPipeError):
                pass
            except BaseException as error:
                self._errors.append((name, error))

        threading.Thread(target=run, daemon=True).start()

    def accept_focus(self, server):
        while True:
            conn, _ = server.accept()
            self.focus = conn

    def serve_commands(self, server):
        while True:
            conn, _ = server.accept()
            with conn:
                command = conn.recv(512)
                self.command_log.append(command)
                replies = {b"j/activewindow": {"class": "Code", "title": "SENSITIVE-WINDOW-TITLE"},
                           b"j/locked": {"locked": self.locked},
                           b"j/monitors": [{"dpmsStatus": self.power_on}]}
                conn.sendall(json.dumps(replies[command]).encode())

    def send(self, obj, opcode, data=b""):
        assert obj is not None
        with self._wayland_write:
            self.wayland.sendall(u32(obj, ((8 + len(data)) << 16) | opcode) + data)

    def send_focus(self, app):
        self.focus.sendall(f"activewindow>>{app},SENSITIVE-WINDOW-TITLE,with,commas\n".encode())

    def serve_wayland(self, server):
        self.wayland, _ = server.accept()
        registry = None
        objects = {}
        while True:
            obj, meta = struct.unpack("=II", read_exact(self.wayland, 8))
            size, op = meta >> 16, meta & 0xFFFF
            assert size >= 8
            data = read_exact(self.wayland, size - 8)
            if obj == 1 and op == 1:  # wl_display.get_registry
                registry, = struct.unpack("=I", data)
                for glob in self.globals:
                    name, version = self.globals[glob]
                    self.send(registry, 0, u32(glob) + wire_string(name) + u32(version))
            elif obj == 1 and op == 0:  # wl_display.sync
                ident, = struct.unpack("=I", data)
                self.send(ident, 0, u32(100))
                self.send(1, 1, u32(ident))
            elif obj == registry and op == 0:  # wl_registry.bind
                glob, = struct.unpack_from("=I", data)
                _, ident = struct.unpack_from("=II", data, len(data) - 8)
                objects[ident] = self.globals[glob][0]
            elif objects.get(obj) == "ext_idle_notifier_v1":
                assert op in (1, 2)
                self.use_input_idle = (op == 2)
                self.idle_id, _, _ = struct.unpack("=III", data)
            elif objects.get(obj) == "hyprland_lock_notifier_v1":
                assert op == 1
                self.lock_id, = struct.unpack("=I", data)
            elif objects.get(obj) == "zwlr_output_power_manager_v1":
                assert op == 0
                ident, _ = struct.unpack("=II", data)
                self.power_ids.append(ident)
                self.send(ident, 0, u32(1))  # initial mode: ON
            else:
                raise AssertionError(f"unsupported Wayland request: {obj} {op} {data!r}")


def available_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def test(binary, root, native=True):
    runtime = root / "run"
    fixture = Compositor(runtime, native)
    port = available_port()
    env = {**os.environ, "HOME": str(root), "XDG_RUNTIME_DIR": str(runtime),
           "XDG_CONFIG_HOME": str(root / "config"), "XDG_DATA_HOME": str(root / "data"),
           "XDG_DATA_DIRS": str(root / "system-data"), "XDG_STATE_HOME": str(root / "state"),
           "WAYLAND_DISPLAY": "wayland-test", "HYPRLAND_INSTANCE_SIGNATURE": "local_test"}
    env.pop("WAYLAND_SOCKET", None)
    config = root / "config" / "irminsul"
    config.mkdir(parents=True)
    (config / "config.toml").write_text(f'port={port}\nidle_seconds=2\ncheckpoint_seconds=15\nexcluded_apps=["privatebrowser"]\n', encoding="utf-8")
    origin = f"http://127.0.0.1:{port}"
    appdata = root / "data" / "irminsul"
    log = root / "daemon.log"

    def request(path, params=None, headers=None):
        args = headers or {}
        data = None if params is None else json.dumps(params).encode()
        if data is not None:
            args = {"Content-Type": "application/json", **args}
        req = urllib.request.Request(origin + path, data=data, headers=args)
        try:
            with urllib.request.urlopen(req, timeout=2) as response:
                return response.status, response.headers, response.read()
        except urllib.error.HTTPError as e:
            return e.code, e.headers, e.read()

    def report(end=None):
        code, _, body = request("/api/v1/report?days=1" + (f"&end={end}" if end else ""))
        assert code == 200, (code, body)
        return json.loads(body)

    def until(predicate, timeout=5):
        end = time.monotonic() + timeout
        last = None
        while time.monotonic() < end:
            if fixture._errors:
                raise AssertionError(fixture._errors)
            try:
                last = report()
                if predicate(last):
                    return last
            except (urllib.error.URLError, ConnectionError):
                pass
            time.sleep(0.025)
        raise AssertionError(f"timed out waiting for report condition: {last}")

    def status(name, *, source=None):
        return until(lambda r: r["session"]["status"] == name and (source is None or r["session"]["display_source"] == source))

    def unchanged(name):
        before = status(name)["total_ms"]
        time.sleep(0.13)
        assert status(name)["total_ms"] == before, name
        return before

    def preference(**payload):
        code, _, result = request("/api/v1/preferences", payload, {"Origin": origin})
        assert code == 200, result

    def stop(proc):
        if proc.poll() is None:
            proc.send_signal(signal.SIGTERM)
        assert proc.wait(timeout=5) == 0

    def start(out):
        return subprocess.Popen([binary, "run"], env=env, stdout=out, stderr=out)

    proc = None
    try:
        with log.open("wb") as out:
            proc = start(out)
            r = status("tracking", source="wayland" if native else "snapshot")
            assert r["session"]["current_app"] == "Visual Studio Code"
            assert fixture.use_input_idle
            if native:
                web = config / "web"
                (web / "extras").mkdir(parents=True)
                (web / "theme.css").write_text(":root {--accent:#155e75}", encoding="utf-8")
                (web / "extras" / "outside.css").symlink_to(config / "config.toml")
                code, _, body = request("/theme.css")
                assert code == 200 and body == b":root {--accent:#155e75}"
                assert request("/extras/outside.css")[0] == 404
            assert request("/api/v1/report", headers={"Host": f"example.org:{port}"})[0] == 403
            assert request("/api/v1/preferences", {"paused": True}, {"Origin": "https://example.org"})[0] == 403
            assert request("/api/v1/preferences", {"paused": True})[0] == 403
            assert request("/api/v1/report?days=99")[0] == 400
            assert request("/api/v1/report?end=2000-1-1")[0] == 400
            assert request("/api/v1/report?end=2100-01-01")[0] == 400
            dupe = subprocess.run([binary, "run"], env=env, capture_output=True, timeout=4)
            assert dupe.returncode != 0 and b"data-directory lock" in dupe.stderr
            time.sleep(0.1)
            fixture.send_focus("org.mozilla.firefox")
            until(lambda r: r["switches"] == 1 and r["session"]["current_app"] == "Firefox")
            time.sleep(0.15)
            r = until(lambda r: len(r["apps"]) == 2)
            assert {app["id"] for app in r["apps"]} == {"code", "firefox"}
            fixture.send_focus("firefox")  # Same app, new window: not another switch.
            assert report()["switches"] == 1
            fixture.send_focus("PrivateBrowser")
            r = until(lambda r: r["unattributed_ms"] >= 20)
            assert all("privatebrowser" not in a["id"] for a in r["apps"])
            fixture.send(fixture.idle_id, 0)
            unchanged("idle")
            if native:
                assert fixture.command_log == [b"j/activewindow"], fixture.command_log
                a = Path(f"/proc/{proc.pid}/stat").read_text().split()[13:15]
                time.sleep(1.05)  # No browser requests while idle.
                b = Path(f"/proc/{proc.pid}/stat").read_text().split()[13:15]
                ticks = sum(map(int, b)) - sum(map(int, a))
                assert ticks <= 6, (a, b)
                proc_status = Path(f"/proc/{proc.pid}/status").read_text().splitlines()
                rss = next(line.split(":", 1)[1].strip() for line in proc_status if line.startswith("VmRSS:"))
                threads = next(line.split(":", 1)[1].strip() for line in proc_status if line.startswith("Threads:"))
                print(f"Mock daemon sample: {rss} RSS, {threads} thread, {ticks} CPU ticks over 1.05 s idle")
            fixture.send(fixture.idle_id, 1)
            status("tracking")
            fixture.send_focus("kitty")
            until(lambda r: r["session"]["current_app"] == "Kitty")
            time.sleep(0.08)
            if native:
                fixture.send(fixture.lock_id, 0)
                unchanged("locked")
                fixture.send(fixture.lock_id, 1)
                status("tracking")
                assert len(fixture.power_ids) == 2
                fixture.send(fixture.power_ids[0], 0, u32(0))
                status("tracking")  # A second display is still on.
                fixture.send(fixture.power_ids[1], 0, u32(0))
                unchanged("display-off")
                fixture.send(fixture.power_ids[0], 0, u32(1))
                status("tracking")
            else:
                fixture.power_on = False
                fixture.focus.sendall(b"monitorremoved>>mock-monitor\n")
                unchanged("display-off")
                fixture.power_on = True
                fixture.focus.sendall(b"monitoradded>>mock-monitor\n")
                status("tracking")
                assert b"j/monitors" in fixture.command_log
            if native:
                fixture.send_focus("Northstar-Window")
                until(lambda r: any(a["id"] == "northstar-window" and a["ms"] > 0 for a in r["apps"]))
                fixture.send_focus("net.imput.Helium")
                r = until(lambda r: any(a["id"] == "net.imput.helium" and a["ms"] > 0 for a in r["apps"]))
                helium = next(a for a in r["apps"] if a["id"] == "net.imput.helium")
                assert (helium["name"], helium["category"], helium["category_source"]) == ("Helium", "Browsing", "built-in")

            preference(goal_minutes=240)
            preference(paused=True)
            saved = unchanged("paused")
            assert saved >= 200
            if native:
                desktop = root / "data" / "applications" / "org.test.Northstar.desktop"
                desktop.parent.mkdir(parents=True)
                desktop.write_text("[Desktop Entry]\nType=Application\nName=Northstar\n"
                                   "StartupWMClass=Northstar-Window\nCategories=Network;WebBrowser;\n",
                                   encoding="utf-8")
                # A launcher added mid-session doesn't require a watcher or poll.
                r = report()
                northstar = next(a for a in r["apps"] if a["id"] == "northstar-window")
                assert northstar["category"] == "Other"
                historical_day = (date.fromisoformat(r["today"]) - timedelta(days=2)).isoformat()
            code, headers, content = request("/api/v1/export?days=1")
            assert code == 200 and "attachment" in headers["Content-Disposition"]
            assert headers["Cache-Control"] == "no-store"
            rows = list(csv.DictReader(io.StringIO(content.decode())))
            assert int(round(sum(float(row["active_seconds"]) for row in rows) * 1000)) == saved
            assert sum(int(row["app_switches"]) for row in rows) == report()["switches"]
            stop(proc)
            db = appdata / "usage.sqlite3"
            assert os.stat(db).st_mode & 0o077 == 0
            with sqlite3.connect(db) as conn:
                assert conn.execute("SELECT sum(ms) FROM usage").fetchone()[0] == saved
                assert conn.execute("SELECT value FROM preferences WHERE name='paused'").fetchone() == (1,)
                if native:
                    # A pre-existing Helium record uses the same schema as before
                    # the update: no name, category, or launcher is stored.
                    conn.execute("INSERT INTO usage VALUES (?, 10, 'helium-browser', 5700000, 4)", (historical_day,))
                    original_usage = conn.execute("SELECT * FROM usage ORDER BY day,hour,app").fetchall()
            assert b"SENSITIVE-WINDOW-TITLE" not in db.read_bytes()
            assert b"privatebrowser" not in db.read_bytes()
            # A full restart still exposes the same history/goal/pause state.
            proc = start(out)
            r = status("paused")
            assert r["goal_minutes"] == 240 and r["total_ms"] == saved
            if native:
                updated = next(a for a in r["apps"] if a["id"] == "northstar-window")
                assert (updated["category"], updated["category_source"], updated["name"]) == ("Browsing", "desktop-entry", "Northstar")
                assert (updated["ms"], updated["switches"]) == (northstar["ms"], northstar["switches"])
                old_report = report(historical_day)
                assert old_report["total_ms"] == 5700000 and old_report["switches"] == 4
                assert old_report["categories"] == [{"name": "Browsing", "ms": 5700000}]
                old_entry, = old_report["apps"]
                assert (old_entry["id"], old_entry["name"], old_entry["category"]) == ("helium-browser", "Helium", "Browsing")
                code, _, body = request(f"/api/v1/export?days=1&end={historical_day}")
                assert code == 200
                row, = csv.DictReader(io.StringIO(body.decode()))
                assert (row["app_id"], row["category"], float(row["active_seconds"])) == ("helium-browser", "Browsing", 5700)
            stop(proc)
            if native:
                with sqlite3.connect(db) as conn:
                    assert original_usage == conn.execute("SELECT * FROM usage ORDER BY day,hour,app").fetchall()
                print("OK: live and historical Helium classification; late launcher visible after restart, with no usage changes")
            if fixture._errors:
                raise AssertionError(fixture._errors)
    except BaseException:
        print(log.read_text(), file=sys.stderr)
        raise
    finally:
        if proc and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=4)
            except subprocess.TimeoutExpired:
                proc.kill()


if __name__ == "__main__":
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/irminsul").resolve(strict=True))
    with tempfile.TemporaryDirectory(prefix="irminsul-test-") as root:
        test(binary, Path(root), native=True)
        print("OK: native focus, idle, lock, dual-display power, pause persistence, export and loopback guards")
    with tempfile.TemporaryDirectory(prefix="irminsul-test-") as root:
        test(binary, Path(root), native=False)
        print("OK: compatibility snapshots when lock and display protocols are not advertised")
