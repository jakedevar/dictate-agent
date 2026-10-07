#!/usr/bin/env python3
"""Synthetic integration tests. Never use the real HOME or user service manager."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import tempfile
import time
import types
import unittest
from contextlib import closing
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parent

# The same stub is installed under all four program names. The old daemon is
# a real, independently signalable process; systemctl never reaches systemd.
STUB = r'''#!/usr/bin/env python3
import json, os, pathlib, signal, subprocess, sys, time, tomllib
home = pathlib.Path(os.environ["HOME"])
cfg = pathlib.Path(os.environ["XDG_CONFIG_HOME"])
run = pathlib.Path(os.environ["XDG_RUNTIME_DIR"])
state = home / "stub"
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
def event(message):
    with (state / "events").open("a") as f: f.write(message + "\n")
def stop(pid):
    try: os.kill(int(pid), signal.SIGTERM)
    except ProcessLookupError: pass
if name in ("dictated", "dictate-agent"):
    if "--check-config" in args:
        sys.exit(1 if (state / "bad-config").exists() else 0)
    is_new = name == "dictated"
    probe = cfg != home / ".config"
    config = args[args.index("--config") + 1] if "--config" in args else str(cfg / "dictate-agent/config.toml")
    if is_new:
        data = tomllib.loads(pathlib.Path(config).read_text())
        event("start-new " + config)
        if probe:
            assert not data["audio"]["capture"]
            assert not data["history"]["enabled"]
            assert not data["history"]["import_python_db"]
            assert data["dictionary"]["db_path"].startswith(str(cfg.parent))
        else:
            (state / "active-config").write_text(config)
    else: event("start-old")
    with (state / "pids").open("a") as f: f.write(str(os.getpid()) + "\n")
    legacy = cfg / "dictate-agent/dictate.pid"
    own = run / "dictate-agent/dictated.pid" if is_new else legacy
    own.parent.mkdir(parents=True, exist_ok=True)
    own.write_text(str(os.getpid()))
    legacy.parent.mkdir(parents=True, exist_ok=True)
    legacy.write_text(str(os.getpid()))
    if is_new:
        sock = pathlib.Path(os.environ.get("DICTATE_SOCKET", str(run / "dictate-agent/dictated.sock")))
        sock.parent.mkdir(parents=True, exist_ok=True)
        # The real daemon binds its socket shortly after writing its PID file.
        if (state / "slow-socket").exists(): time.sleep(0.6)
        sock.touch()
    def term(sig, frame):
        event("term-" + name)
        for p in (own, legacy):
            if p.exists() and p.read_text() == str(os.getpid()): p.unlink()
        sys.exit(0)
    signal.signal(signal.SIGTERM, term)
    signal.signal(signal.SIGUSR1, lambda *_: event("toggle-" + name))
    while True: time.sleep(0.1)
elif name == "systemctl":
    event("systemctl " + " ".join(args))
    assert args[0] == "--user"
    cmd = args[1]
    active = state / "active"
    if cmd == "is-active": sys.exit(0 if active.exists() else 3)
    if cmd == "enable":
        if (state / "bad-enable").exists(): sys.exit(1)
        drop = cfg / "systemd/user/dictated.service.d/90-dictate-cutover.conf"
        line = [x for x in drop.read_text().splitlines() if x.startswith("ExecStart=") and x != "ExecStart="][0]
        import shlex
        command = shlex.split(line.removeprefix("ExecStart="))
        with (state / "service.log").open("a") as log:
            child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
        active.write_text(str(child.pid))
    elif cmd == "disable":
        if active.exists():
            stop(active.read_text())
            deadline = time.monotonic() + 5
            while (run / "dictate-agent/dictated.pid").exists() and time.monotonic() < deadline: time.sleep(0.02)
            active.unlink()
    elif cmd != "daemon-reload": raise AssertionError(cmd)
elif name == "dictate":
    socket = pathlib.Path(os.environ.get("DICTATE_SOCKET", str(run / "dictate-agent/dictated.sock")))
    probe = not str(socket).startswith(str(home))
    if args[0] == "status":
        sys.exit(1 if (state / "bad-status").exists() or not socket.exists() else 0)
    elif args[0] == "doctor":
        event("doctor " + ("probe" if probe else "service") + " " + " ".join(args))
        if (state / "bad-json").exists(): print("invalid"); sys.exit(1)
        checks = [{"id": "daemon", "status": "ok", "detail": "synthetic"}]
        flag = "preflight-fail" if probe else "postflight-fail"
        if (state / flag).exists() and (probe or "--quick" not in args):
            checks.append({"id": "stt_model", "status": "fail", "detail": "Ollama is unrelated"})
        if (state / "ollama-fail").exists(): checks.append({"id": "ollama", "status": "fail"})
        if (state / "formatter-fail").exists(): checks.append({"id": "formatter", "status": "fail"})
        print(json.dumps({"checks": checks}))
        sys.exit(int(any(x["status"] == "fail" for x in checks)))
    elif args[:2] == ["dict", "list"]:
        p = state / "dictionary"
        print(json.dumps({"entries": json.loads(p.read_text()) if p.exists() else []}))
    elif args[:2] == ["dict", "add"]:
        event("dictionary-add")
        (state / "dictionary").write_text(json.dumps([{"phrase": args[2]}]))
    else: raise AssertionError(args)
'''


class CutoverTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cutover-test-", dir="/tmp")
        self.home = Path(self.temp.name) / "home"
        self.state = self.home / "stub"
        self.bin = self.home / ".local/bin"
        self.cfg = self.home / ".config"
        self.data = self.home / ".local/share"
        self.run = self.home / "run"
        for p in (self.state, self.bin, self.cfg / "dictate-agent", self.data / "dictate-agent", self.run):
            p.mkdir(parents=True, exist_ok=True)
        self.env = dict(os.environ, HOME=str(self.home), XDG_CONFIG_HOME=str(self.cfg),
                        XDG_DATA_HOME=str(self.data), XDG_RUNTIME_DIR=str(self.run),
                        CUTOVER_READY_SECS="3",
                        PATH=str(self.bin) + os.pathsep + os.environ["PATH"])
        self.env.pop("DICTATE_SOCKET", None)
        self.env.pop("DBUS_SESSION_BUS_ADDRESS", None)
        for name in ("dictated", "dictate", "dictate-agent", "systemctl"):
            p = self.bin / name
            p.write_text(STUB)
            p.chmod(0o700)
        unit = self.cfg / "systemd/user/dictated.service"
        unit.parent.mkdir(parents=True)
        unit.write_text("synthetic unit\n")
        self.config = self.cfg / "dictate-agent/config.toml"
        self.original = '# synthetic legacy config\n[whisper]\nlanguage = "auto"\n[history]\nimport_python_db = false\n'
        self.config.write_text(self.original)
        self.db = self.data / "dictate-agent/history.db"
        self.connection = sqlite3.connect(self.db)
        self.connection.execute("PRAGMA journal_mode=WAL")
        self.connection.execute("CREATE TABLE synthetic(value TEXT)")
        self.connection.execute("INSERT INTO synthetic VALUES ('fixture')")
        self.connection.commit()
        self.old = subprocess.Popen([str(self.bin / "dictate-agent")], env=self.env,
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.wait_pid("dictate-agent", self.cfg / "dictate-agent/dictate.pid")

    def tearDown(self):
        # Only PIDs recorded by our stubs; never pgrep or a real user process.
        pids = self.state / "pids"
        if pids.exists():
            for raw in pids.read_text().splitlines():
                try:
                    root = Path(f"/proc/{raw}")
                    if os.fsencode("HOME=" + str(self.home)) in root.joinpath("environ").read_bytes().split(b"\0"):
                        os.kill(int(raw), signal.SIGTERM)
                except (ProcessLookupError, FileNotFoundError):
                    pass
        self.old.wait(timeout=5)
        self.connection.close()
        self.temp.cleanup()

    def wait_pid(self, binary, path):
        subprocess.run(["python3", str(SCRIPTS / "cutover-support.py"), "wait", "claim", str(path), str(self.bin / binary)],
                       env=self.env, check=True, capture_output=True, timeout=25)

    def script(self, rollback=False, args=(), success=True, env=None):
        script = SCRIPTS / ("cutover-rollback.sh" if rollback else "cutover.sh")
        result = subprocess.run(["bash", str(script), *args], env=env or self.env,
                                capture_output=True, text=True, timeout=50)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result

    def events(self):
        return (self.state / "events").read_text()

    def assert_old(self):
        self.wait_pid("dictate-agent", self.cfg / "dictate-agent/dictate.pid")
        self.assertFalse((self.state / "active").exists())

    def backups(self):
        return sorted((self.cfg / "dictate-agent/cutover").glob("*/config.toml.pre-cutover"))

    def test_cutover_and_rerun_preserve_originals_and_backups(self):
        self.script()
        self.assertEqual(self.old.wait(timeout=5), 0)
        self.assertEqual(self.config.read_text(), self.original)
        backup = self.backups()[0]
        self.assertEqual(backup.read_text(), self.original)
        with closing(sqlite3.connect(backup.with_name("history.db.pre-cutover"))) as db:
            self.assertEqual(db.execute("SELECT value FROM synthetic").fetchall(), [("fixture",)])
        active = (self.state / "active").read_text()
        self.script()
        self.assertEqual((self.state / "active").read_text(), active)
        self.assertEqual(len(self.backups()), 1)
        self.assertEqual(backup.read_text(), self.original)
        self.assertEqual(self.connection.execute("SELECT value FROM synthetic").fetchall(), [("fixture",)])

    def test_rollback_and_repeated_rollback(self):
        self.script()
        self.script(rollback=True)
        self.assert_old()
        pid = (self.cfg / "dictate-agent/dictate.pid").read_text()
        self.script(rollback=True)
        self.assertEqual((self.cfg / "dictate-agent/dictate.pid").read_text(), pid)
        self.assertFalse((self.cfg / "systemd/user/dictated.service.d/90-dictate-cutover.conf").exists())

    def test_failed_postflight_rolls_back(self):
        (self.state / "postflight-fail").touch()
        self.script(success=False)
        self.assert_old()
        self.assertEqual(self.config.read_text(), self.original)

    def test_socket_bound_after_pid_file_still_cuts_over(self):
        # 2026-10-07 live run: the PID file appeared ~0.1 s before the socket
        # and a single `dictate status` raced it, rolling a healthy cutover back.
        (self.state / "slow-socket").touch()
        self.script()
        self.assertIn("systemctl --user enable --now dictated", self.events())

    def test_failed_status_rolls_back(self):
        (self.state / "bad-status").touch()
        self.script(success=False)
        self.assert_old()

    def test_failed_enable_rolls_back(self):
        (self.state / "bad-enable").touch()
        self.script(success=False)
        self.assert_old()

    def test_preflight_failure_leaves_old_running(self):
        (self.state / "preflight-fail").touch()
        self.script(success=False)
        self.assertIsNone(self.old.poll())
        self.assertNotIn("systemctl --user enable", self.events())
        self.assertEqual(self.backups(), [])

    def test_bad_config_never_stops_old(self):
        (self.state / "bad-config").touch()
        self.script(success=False)
        self.assertIsNone(self.old.poll())

    def test_invalid_diagnostics_never_stops_old(self):
        (self.state / "bad-json").touch()
        self.script(success=False)
        self.assertIsNone(self.old.poll())

    def test_ollama_exception_is_preflight_only(self):
        (self.state / "ollama-fail").touch()
        self.script(success=False)
        self.assertIn("systemctl --user enable --now dictated", self.events())
        self.assert_old()

    def test_unrelated_formatter_failure_stops_preflight(self):
        (self.state / "formatter-fail").touch()
        self.script(success=False)
        self.assertIsNone(self.old.poll())
        self.assertNotIn("systemctl --user enable", self.events())

    def test_failed_postflight_on_rerun_rolls_back(self):
        self.script()
        (self.state / "postflight-fail").touch()
        self.script(success=False)
        self.assert_old()

    def test_optional_flags_use_generated_copies(self):
        self.script(args=("--import-history", "--language", "en", "--claude-dictionary"))
        import tomllib
        active = Path((self.state / "active-config").read_text())
        self.assertTrue(tomllib.loads(active.read_text())["history"]["import_python_db"])
        self.assertEqual(tomllib.loads(active.read_text())["whisper"]["language"], "en")
        self.assertFalse(tomllib.loads(active.with_name("steady.toml").read_text())["history"]["import_python_db"])
        dropin = self.cfg / "systemd/user/dictated.service.d/90-dictate-cutover.conf"
        self.assertIn("steady.toml", dropin.read_text())
        self.assertEqual(self.events().count("dictionary-add"), 1)
        self.assertEqual(self.config.read_text(), self.original)

    def test_restore_is_opt_in_and_saves_replaced_config(self):
        self.script()
        self.config.write_text(self.original + "# later edit\n")
        self.script(rollback=True)
        self.assertIn("later edit", self.config.read_text())
        self.script(rollback=True, args=("--restore-config",))
        self.assertEqual(self.config.read_text(), self.original)
        saved = list((self.cfg / "dictate-agent/cutover").glob("restore-*/config.toml.before-restore"))
        self.assertEqual(len(saved), 1)
        self.assertIn("later edit", saved[0].read_text())

    def test_second_cutover_never_overwrites_first_backup(self):
        self.script()
        first = self.backups()[0]
        self.script(rollback=True)
        self.config.write_text(self.original + "# second cutover\n")
        self.script()
        self.assertEqual(len(self.backups()), 2)
        self.assertEqual(first.read_text(), self.original)
        self.script(rollback=True, args=("--restore-config",))
        self.assertIn("second cutover", self.config.read_text())

    def test_invalid_pid_uses_verified_pgrep_fallback(self):
        (self.cfg / "dictate-agent/dictate.pid").write_text("0")
        self.script()
        self.assertEqual(self.old.wait(timeout=5), 0)

    def test_unowned_dropin_refused(self):
        p = self.cfg / "systemd/user/dictated.service.d/90-dictate-cutover.conf"
        p.parent.mkdir(parents=True)
        p.write_text("user config\n")
        self.script(success=False)
        self.script(rollback=True, success=False)
        self.assertEqual(p.read_text(), "user config\n")
        self.assertIsNone(self.old.poll())

    def test_xdg_escape_refused_by_both_scripts(self):
        env = dict(self.env, XDG_DATA_HOME="/tmp/outside-temporary-home")
        self.script(success=False, env=env)
        self.script(rollback=True, success=False, env=env)
        self.assertNotIn("systemctl", self.events())

    def test_real_home_guard_with_synthetic_account_home(self):
        spec = importlib.util.spec_from_file_location("support", SCRIPTS / "cutover-support.py")
        support = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(support)
        with patch.dict(os.environ, self.env), patch.object(support.pwd, "getpwuid", return_value=types.SimpleNamespace(pw_dir=str(self.home))):
            with self.assertRaisesRegex(ValueError, "refusing the real HOME"):
                support.guard(False)

    def test_shared_history_path_is_redirected(self):
        self.config.write_text(self.original + 'db_path = ' + json.dumps(str(self.db)) + '\n')
        self.script()
        import tomllib
        active = Path((self.state / "active-config").read_text())
        self.assertEqual(tomllib.loads(active.read_text())["history"]["db_path"], str(self.data / "dictated/history.db"))

    def test_missing_installed_binary_refuses_cutover(self):
        (self.bin / "dictate").unlink()
        self.script(success=False)
        self.assertIsNone(self.old.poll())

    def test_missing_unit_refuses_cutover(self):
        (self.cfg / "systemd/user/dictated.service").unlink()
        self.script(success=False)
        self.assertIsNone(self.old.poll())

    def test_unrelated_pid_is_never_signaled(self):
        foreign = subprocess.Popen(["sleep", "30"], env=self.env)
        try:
            (self.cfg / "dictate-agent/dictate.pid").write_text(str(foreign.pid))
            self.script()
            self.assertIsNone(foreign.poll())
            self.assertEqual(self.old.wait(timeout=5), 0)
        finally:
            foreign.terminate()
            foreign.wait(timeout=5)

    def test_existing_dictionary_entry_is_preserved(self):
        # Synthetic fixture, not an export of anyone's personal vocabulary.
        canonical = "Claude"
        original = json.dumps([{"phrase": canonical, "synthetic": "keep"}])
        (self.state / "dictionary").write_text(original)
        self.script(args=("--claude-dictionary",))
        self.assertNotIn("dictionary-add", self.events())
        self.assertEqual((self.state / "dictionary").read_text(), original)


if __name__ == "__main__":
    unittest.main(verbosity=2)
