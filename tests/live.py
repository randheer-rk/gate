#!/usr/bin/env python3
"""Opt-in smoke test against a running OpenCode service; leaves evidence in /tmp."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from smoke import BINARY, Mcp


def run():
    root = Path(tempfile.mkdtemp(prefix="gate-live-"))
    repo = root / "repo"
    repo.mkdir()
    (repo / "opencode.json").write_text(json.dumps({"permission": {"edit": "ask", "bash": "ask"}}))
    (repo / "seed.txt").write_text("initial\n")
    subprocess.run(["git", "init", "--quiet", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "add", "."], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=Gate Test", "-c", "user.email=gate@example.invalid",
                    "commit", "--quiet", "-m", "live smoke fixture"], check=True)
    env = {"GATE_DIRECTORY": str(repo), "GATE_STATE_DIR": str(root / "jobs"),
           "GATE_ARTIFACT_DIR": str(root / "artifacts"), "GATE_PERMISSION_MODE": "ask"}
    print(f"evidence: {root}", flush=True)
    doctor = subprocess.run([str(BINARY), "doctor", "--json"], env={**os.environ, **env}, capture_output=True, text=True)
    assert doctor.returncode == 0, doctor.stdout + doctor.stderr
    with Mcp(env) as mcp:
        models = mcp.tool("models")["models"]
        model = os.environ.get("GATE_LIVE_MODEL", "opencode/crouton-alpha-free")
        assert model in {m["model"] for m in models}, f"model unavailable: {model}"
        job = mcp.tool("delegate", {
            "task": "Create exactly one new file gate-smoke.txt containing the line 'gate smoke ok'. Use the file editing tool directly. Do not run shell commands or change any other file. Then reply exactly 'gate smoke ok'.",
            "model": model, "agent": "build", "worktree": True, "wait_ms": 0,
        })
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            job = mcp.tool("wait", {"job_id": job["job_id"], "wait_ms": 1000})
            if job["status"] != "running":
                break
        assert job["status"] == "blocked_on_permission", job
        print(f"permission bridge reached: {job['pending_permission']['action']}; restarting Gate", flush=True)
    with Mcp(env) as mcp:
        assert job["job_id"] in {item["job_id"] for item in mcp.tool("list")["jobs"]}
        job = mcp.tool("reply_permission", {"job_id": job["job_id"], "reply": "once"})
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            job = mcp.tool("wait", {"job_id": job["job_id"], "wait_ms": 1000})
            if job["status"] == "blocked_on_permission":
                assert job["pending_permission"]["action"] == "edit", job
                job = mcp.tool("reply_permission", {"job_id": job["job_id"], "reply": "once"})
            if job["status"] in ("completed", "failed", "cancelled"):
                break
        (root / "snapshot.json").write_text(json.dumps(job, indent=2))
        assert job["status"] == "completed", job
        assert not job["error"], job
        assert (Path(job["directory"]) / "gate-smoke.txt").read_text().strip() == "gate smoke ok"
        assert not (repo / "gate-smoke.txt").exists()
        diff = mcp.tool("diff", {"job_id": job["job_id"]})
        (root / "diff.json").write_text(json.dumps(diff, indent=2))
        assert any(file["file"].endswith("gate-smoke.txt") and "gate smoke ok" in file.get("patch", "") for file in diff["files"]), diff
        artifact = json.loads(Path(job["artifact"]).read_text())
        assert artifact["messages"] and artifact["diff"]
        print(f"ok: live edit, permission reply, restart recovery, worktree isolation, diff, artifact; actual model {job['model']}", flush=True)


if __name__ == "__main__":
    run()
