#!/usr/bin/env python3
"""Real daemon + CLI round trip, with mock hardware and synthetic data only.
Run from the repository root after building dictate and dictionary_daemon:
  PATH=/opt/cuda/bin:$PATH cargo build -p dictate-cli --bin dictate
  PATH=/opt/cuda/bin:$PATH cargo build -p dictated --example dictionary_daemon
  python3 scripts/test-dictionary-cli.py
"""
import json
import os
from pathlib import Path
import select
import subprocess
import tempfile

repo = Path(__file__).resolve().parent.parent
target = Path(os.environ.get("CARGO_TARGET_DIR", repo / "target"))
with tempfile.TemporaryDirectory(prefix="s22-cli-", dir="/tmp") as root:
    env = dict(os.environ, XDG_DATA_HOME=f"{root}/data", XDG_RUNTIME_DIR=f"{root}/runtime",
               DICTATE_SOCKET=f"{root}/dictated.sock")
    daemon = subprocess.Popen([str(target / "debug/examples/dictionary_daemon"), root],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True, env=env)
    count = 0
    def cli(*args, success=True):
        global count
        result = subprocess.run([str(target / "debug/dictate"), "dict", *args],
                                capture_output=True, text=True, env=env, timeout=10)
        count += 1
        assert (result.returncode == 0) == success, (args, result.returncode, result.stderr)
        return result.stdout
    def entries():
        return json.loads(cli("list", "--json"))["entries"]
    try:
        assert select.select([daemon.stdout], [], [], 10)[0], "fixture startup timed out"
        ready = daemon.stdout.readline()
        assert ready.startswith("ready "), (ready, daemon.stderr.read() if daemon.poll() is not None else "")
        assert entries() == []
        cli("add", "Kubernetes", "--sounds-like", "cube ernetties,kubernetties", "--app", "slack")
        first = entries()[0]
        assert first["phrase"] == "Kubernetes" and first["apps"] == ["slack"]
        assert first["sounds_like"] == ["cube ernetties", "kubernetties"]
        cli("disable", str(first["id"]))
        assert not entries()[0]["enabled"]
        cli("enable", "Kubernetes")
        assert entries()[0]["enabled"]
        exported = Path(root) / "dictionary.jsonl"
        cli("export", str(exported))
        assert json.loads(exported.read_text())["phrase"] == "Kubernetes"
        cli("rm", "Kubernetes")
        assert entries() == []
        cli("import", str(exported))
        assert entries()[0]["phrase"] == "Kubernetes"
        proposals = json.loads(cli("suggest", "--json"))["suggestions"]
        assert len(proposals) == 1 and proposals[0]["entry"]["phrase"] == "Tauri"
        assert proposals[0]["count"] == 3 and proposals[0]["days"] == 2
        cli("accept", "Tauri")
        assert {e["phrase"] for e in entries()} == {"Kubernetes", "Tauri"}
        cli("import", str(exported))  # updating by phrase preserves the current id
        assert len(entries()) == 2
        bad = Path(root) / "bad.jsonl"
        bad.write_text('{"phrase":"ShouldNotInstall"}\ninvalid json\n')
        cli("import", str(bad), success=False)
        assert len(entries()) == 2
        print(f"CLI round trip: {count} commands passed")
    finally:
        daemon.stdin.close()
        try:
            daemon.wait(timeout=10)
        except subprocess.TimeoutExpired:
            daemon.kill()
            daemon.wait(timeout=10)
        assert daemon.returncode == 0, daemon.stderr.read()
