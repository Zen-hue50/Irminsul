#!/usr/bin/env python3
"""Exercise the rename and installer with isolated, synthetic home directories."""
import fcntl
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def execute(args, env, expected=0):
    out = subprocess.run(args, env=env, cwd=ROOT, capture_output=True, text=True, timeout=160)
    if out.returncode != expected:
        raise AssertionError((out.returncode, out.stdout, out.stderr))
    return out


def verify(binary, root, mode):
    config, data, state, bin_dir = [root / name for name in ("config", "data", "state", "user's bin")]
    env = {**os.environ, "HOME": str(root), "XDG_CONFIG_HOME": str(config), "XDG_DATA_HOME": str(data),
           "XDG_STATE_HOME": str(state), "IRMINSUL_BIN_DIR": str(bin_dir)}
    old_config, old_data = config / "stilltime", data / "stilltime"
    web = old_config / "web"
    web.mkdir(parents=True)
    (web / "index.html").write_text("Earlier HTML including my custom text")
    (web / "app.js").write_text("// my customized earlier script")
    (web / "theme.css").write_text(":root { --radius: 8px; }")
    (web / "stilltime.svg").write_text('<svg xmlns="http://www.w3.org/2000/svg"/>')
    settings = 'port=9876\nidle_seconds=120\n'
    (old_config / "config.toml").write_text(settings)
    old_data.mkdir(parents=True)
    with sqlite3.connect(old_data / "usage.sqlite3") as db:
        db.executescript("CREATE TABLE usage(day TEXT, hour INTEGER, app TEXT, ms INTEGER, switches INTEGER);"
                         "CREATE TABLE preferences(name TEXT, value INTEGER);"
                         "INSERT INTO usage VALUES ('2026-11-18',14,'code',3200,1);"
                         "INSERT INTO preferences VALUES ('goal_minutes',240), ('paused',1);")
    lock = old_data / "daemon.lock"
    with lock.open("w") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        result = execute([binary, "migrate"], env, 1)
        assert "previous daemon is running" in result.stderr
        assert old_config.is_dir() and old_data.is_dir() and not (config / "irminsul").exists()
        assert (web / "index.html").is_file()
    conflict = config / "irminsul"
    conflict.mkdir()
    result = execute([binary, "migrate"], env, 1)
    assert "Neither was changed" in result.stderr
    conflict.rmdir()

    hypr = config / "hypr"
    hypr.mkdir()
    compositor = hypr / f"hyprland.{mode}"
    actual = hypr / "home-managed-file"
    prefix = "--" if mode == "lua" else "#"
    old_hook = f"{prefix} BEGIN STILLTIME AUTOSTART\nold command\n{prefix} END STILLTIME AUTOSTART\n"
    original = f"{prefix} First custom line\n{old_hook}{prefix} Last custom line\n"
    actual.write_text(original)
    if mode == "lua":
        compositor.symlink_to(actual.name)
    else:
        actual.rename(compositor)
    bin_dir.mkdir()
    old_binary = bin_dir / "stilltime"
    old_binary.write_text("#!/bin/sh\nexit 0\n")
    old_binary.chmod(0o755)

    execute([str(ROOT / "install.sh"), "--no-start"], env)
    content = compositor.read_text()
    assert content.startswith(f"{prefix} First custom line\n{prefix} BEGIN IRMINSUL AUTOSTART\n")
    assert content.endswith(f"{prefix} END IRMINSUL AUTOSTART\n{prefix} Last custom line\n")
    assert "old command" not in content and "STILLTIME" not in content
    assert content.count("BEGIN IRMINSUL AUTOSTART") == 1
    config_backup = Path(f"{compositor}.irminsul.bak")
    assert config_backup.read_text() == original
    assert compositor.is_symlink() == (mode == "lua")
    assert not old_binary.exists() and (state / "irminsul" / "previous-executable").is_file()
    installed = bin_dir / "irminsul"
    version = execute([binary, "--version"], env).stdout
    assert version.startswith("irminsul ")
    assert execute([str(installed), "--version"], env).stdout == version
    if mode == "conf":
        command = next(line for line in content.splitlines() if line.startswith("exec-once = ")).split(" = ", 1)[1]
        assert command.endswith(" run")
        assert execute(["/bin/sh", "-c", command[:-4] + " --version"], env).stdout == version
    assert not old_config.exists() and not old_data.exists()
    assert (config / "irminsul" / "config.toml").read_text() == settings
    new_web = config / "irminsul" / "web"
    assert (new_web / "theme.css").read_text() == ":root { --radius: 8px; }"
    assert not (new_web / "index.html").exists() and not (new_web / "app.js").exists()
    assert (new_web / "irminsul.svg").is_file()
    prior = config / "irminsul" / "previous-dashboard"
    assert "my custom text" in (prior / "index.html").read_text()
    assert "my customized earlier script" in (prior / "app.js").read_text()
    with sqlite3.connect(data / "irminsul" / "usage.sqlite3") as db:
        assert db.execute("SELECT app,ms,switches FROM usage").fetchone() == ("code",3200,1)
        assert db.execute("SELECT value FROM preferences WHERE name='goal_minutes'").fetchone() == (240,)
    execute([str(installed), "theme", "init"], env)
    assert '<title>Irminsul</title>' in (new_web / "index.html").read_text()
    assert (new_web / "palettes.css").is_file()
    assert (new_web / "theme.css").read_text() == ":root { --radius: 8px; }"
    execute([str(ROOT / "install.sh"), "--no-start"], env)
    assert compositor.read_text() == content and config_backup.read_text() == original


if __name__ == "__main__":
    binary = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/irminsul").resolve(strict=True))
    for mode in ("lua", "conf"):
        with tempfile.TemporaryDirectory(prefix="irminsul-upgrade-") as tmp:
            verify(binary, Path(tmp), mode)
        print(f"OK: {mode} upgrade, preserved database and overrides, autostart replacement, lock/conflict guard and idempotence")
