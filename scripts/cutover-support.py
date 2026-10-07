#!/usr/bin/env python3
"""Local-only helpers for the cutover shell scripts (Python 3.11+)."""
import datetime
import json
import math
import os
from pathlib import Path
import pwd
import sqlite3
import sys
import time
import tomllib
from contextlib import closing


def guard(live):
    home = Path(os.environ["HOME"]).resolve()
    real = Path(pwd.getpwuid(os.getuid()).pw_dir).resolve()
    if not live:
        if home == real:
            raise ValueError("refusing the real HOME; operator cutover requires --live")
        for key in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_RUNTIME_DIR"):
            value = os.environ.get(key)
            if not value or not Path(value).is_absolute() or not Path(value).resolve().is_relative_to(home):
                raise ValueError(f"isolated run requires {key} under the temporary HOME")
        for base, suffix in (("XDG_CONFIG_HOME", "dictate-agent"),
                             ("XDG_CONFIG_HOME", "systemd/user"),
                             ("XDG_DATA_HOME", "dictate-agent"),
                             ("XDG_DATA_HOME", "dictated"),
                             ("XDG_RUNTIME_DIR", "dictate-agent")):
            if not (Path(os.environ[base]) / suffix).resolve().is_relative_to(home):
                raise ValueError(f"{base}/{suffix} escapes the temporary HOME through a symlink")
        socket = os.environ.get("DICTATE_SOCKET")
        if socket and not Path(socket).resolve().is_relative_to(home):
            raise ValueError("DICTATE_SOCKET must be under the temporary HOME")


def value(v):
    if isinstance(v, str):
        return json.dumps(v, ensure_ascii=False)
    if isinstance(v, bool):
        return str(v).lower()
    if isinstance(v, (int, float)):
        if isinstance(v, float) and not math.isfinite(v):
            return str(v).lower()
        return repr(v)
    if isinstance(v, (datetime.datetime, datetime.date, datetime.time)):
        return v.isoformat()
    if isinstance(v, list):
        return "[" + ", ".join(value(x) for x in v) + "]"
    if isinstance(v, dict):
        return "{ " + ", ".join(f"{value(k)} = {value(x)}" for k, x in v.items()) + " }"
    raise ValueError(f"unsupported TOML value {type(v)}")


def config(source, destination, mode, language, import_history):
    data = tomllib.loads(Path(source).read_text()) if Path(source).exists() else {}
    history = data.setdefault("history", {})
    # A Python-era explicit path must never make the new daemon write the old DB.
    legacy = Path(os.environ["XDG_DATA_HOME"]) / "dictate-agent/history.db"
    configured = history.get("db_path", "")
    if configured and Path(os.path.expanduser(configured)).resolve() == legacy.resolve():
        history["db_path"] = str(Path(os.environ["XDG_DATA_HOME"]) / "dictated/history.db")
    history["import_python_db"] = import_history == "true"
    if language:
        data.setdefault("whisper", {})["language"] = language
    if mode == "probe":
        history.update(enabled=False, import_python_db=False)
        data.setdefault("dictionary", {})["db_path"] = str(Path(destination).parent / "dictionary.db")
        data.setdefault("audio", {})["capture"] = False
        data.setdefault("hotkey", {})["enabled"] = False
        data.setdefault("notifications", {})["enabled"] = False
    lines = []

    def table(items, path=()):
        if path:
            lines.append("[" + ".".join(value(x) for x in path) + "]")
        for key, item in items.items():
            if not isinstance(item, dict):
                lines.append(f"{value(key)} = {value(item)}")
        for key, item in items.items():
            if isinstance(item, dict):
                table(item, (*path, key))

    table(data)
    output = "\n".join(lines) + "\n"
    tomllib.loads(output)
    # Generated copies only. Never rewrite the source config.
    with open(destination, "x") as f:
        f.write(output)


def matches(pid, binary):
    try:
        pid = int(pid)
        if pid <= 1:
            return False
        root = Path(f"/proc/{pid}")
        if root.stat().st_uid != os.getuid():
            return False
        if root.joinpath("stat").read_text().rsplit(")", 1)[1].split()[0] == "Z":
            return False
        env = root.joinpath("environ").read_bytes().split(b"\0")
        if os.fsencode("HOME=" + os.environ["HOME"]) not in env:
            return False
        args = root.joinpath("cmdline").read_bytes().split(b"\0")
        target = Path(binary).resolve()
        return root.joinpath("exe").resolve() == target or args[1:2] == [os.fsencode(str(target))]
    except (OSError, ValueError, IndexError):
        return False


def pid_from_file(path, binary):
    try:
        pid = Path(path).read_text().strip()
        return pid if matches(pid, binary) else None
    except OSError:
        return None


def wait_for(mode, target, binary):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if mode == "claim":
            pid = pid_from_file(target, binary)
            if pid:
                print(pid)
                return
        else:
            try:
                os.kill(int(target), 0)
                stat = Path(f"/proc/{target}/stat").read_text().rsplit(")", 1)[1].split()[0]
                if stat == "Z":
                    return
            except (ProcessLookupError, FileNotFoundError):
                return
        time.sleep(0.1)
    raise ValueError(f"timed out waiting for {mode}: {target}; no SIGKILL was sent")


def doctor(path, rc, preflight):
    report = json.loads(Path(path).read_text())
    checks = report.get("checks")
    if not isinstance(checks, list) or not checks or int(rc) not in (0, 1):
        raise ValueError("doctor did not return a valid diagnostics report")
    failures = []
    # Only Ollama's named checks are exempt at preflight; unrelated failures
    # mentioning Ollama in their prose are still failures.
    allowed = {"ollama", "grammar_model"} if preflight else set()
    if preflight and any(c.get("id") in allowed and c.get("status") == "fail" for c in checks):
        allowed.add("formatter")
    for check in checks:
        if check.get("status") not in {"ok", "warn", "fail", "skipped"}:
            raise ValueError("doctor returned an unknown check status")
        print(f"{check['status']}: {check['id']}: {check.get('detail', '')}")
        if check["status"] == "fail" and check["id"] not in allowed:
            failures.append(check["id"])
    if not any(c.get("id") == "daemon" and c.get("status") == "ok" for c in checks):
        failures.append("daemon")
    if failures or (int(rc) == 1 and not any(c["status"] == "fail" for c in checks)):
        raise ValueError("doctor failed: " + ", ".join(failures))


def main():
    command, *args = sys.argv[1:]
    if command == "guard":
        guard(args[0] == "true")
    elif command == "config":
        config(*args)
    elif command == "pid":
        pid = pid_from_file(*args)
        if not pid:
            sys.exit(1)
        print(pid)
    elif command == "match":
        sys.exit(0 if matches(*args) else 1)
    elif command == "wait":
        wait_for(*args)
    elif command == "backup-db":
        source, destination = map(Path, args)
        # SQLite backup includes committed WAL rows; cp of the main file does not.
        with closing(sqlite3.connect(source.resolve().as_uri() + "?mode=ro", uri=True)) as src:
            with closing(sqlite3.connect(destination)) as dst:
                src.backup(dst)
    elif command == "doctor":
        doctor(args[0], args[1], args[2] == "preflight")
    elif command == "has-entry":
        entries = json.loads(Path(args[0]).read_text())["entries"]
        sys.exit(0 if any(e["phrase"].casefold() == args[1].casefold() for e in entries) else 1)
    elif command == "dropin":
        binary, config_path = args
        def quote(s):
            # systemd's quoting, specifier expansion and ExecStart $ expansion.
            if "\n" in s or "\r" in s:
                raise ValueError("newline in service path")
            return '"' + s.replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%").replace("$", "$$") + '"'
        print("# Managed by dictate cutover; remove with cutover-rollback.sh")
        print("[Service]\nExecStart=")
        print(f"ExecStart={quote(binary)} --config {quote(config_path)}")
    else:
        raise ValueError(f"unknown helper {command}")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, sqlite3.Error) as error:
        print(f"cutover: {error}", file=sys.stderr)
        sys.exit(1)
