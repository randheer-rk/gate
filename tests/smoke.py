#!/usr/bin/env python3
"""Exercise the built stdio server against a local, authenticated OpenCode fixture."""

import base64
import contextlib
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse


ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("GATE_TEST_BINARY", ROOT / "target/debug/gate"))


class Mcp:
    def __init__(self, env):
        self.stderr = tempfile.TemporaryFile()
        self.process = subprocess.Popen(
            [str(BINARY), "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=self.stderr, text=True, env={**os.environ, **env},
        )
        self.responses = queue.Queue()
        self.counter = 0

        def read():
            try:
                for line in self.process.stdout:
                    self.responses.put(json.loads(line))
            except Exception as error:
                self.responses.put(error)
            self.responses.put(EOFError("MCP server closed stdout"))

        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()
        self.rpc("initialize", {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "gate-smoke", "version": "1"},
        })
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def send(self, message):
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def rpc(self, method, params=None, timeout=15):
        self.counter += 1
        self.send({"jsonrpc": "2.0", "id": self.counter, "method": method, "params": params or {}})
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            response = self.responses.get(timeout=max(0.01, deadline - time.monotonic()))
            if isinstance(response, Exception):
                self.stderr.seek(0)
                raise AssertionError(self.stderr.read().decode()) from response
            if response.get("id") == self.counter:
                assert "error" not in response, response
                return response["result"]
        raise AssertionError(f"MCP timed out: {method}")

    def tool(self, name, args=None, error=False):
        response = self.rpc("tools/call", {"name": name, "arguments": args or {}})
        text = "\n".join(block.get("text", "") for block in response.get("content", []))
        if error:
            assert response.get("isError"), response
            return text
        assert not response.get("isError"), text
        return json.loads(text)

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.reader.join(timeout=1)
        self.process.stdout.close()
        self.stderr.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


class Backend:
    def __init__(self, directory):
        self.directory = directory
        self.sessions = {}
        self.streams = []
        self.lock = threading.RLock()
        self.reject_failures = 0
        self.replies = []
        self.diffs = []

    def event(self, session, kind, **data):
        event = {"type": kind, "location": {"directory": session["location"]["directory"]},
                 "data": {"sessionID": session["id"], **data}}
        for stream in list(self.streams):
            stream.put(event)

    def complete(self, session, outcome="succeeded", emit=True, active=False):
        with self.lock:
            session["active"] = active
            session["outcome"] = outcome
            session["pending"] = []
            session["messages"].append({
                "id": f"msg_a{len(session['messages'])}", "type": "assistant",
                "content": [{"type": "text", "text": "done"}], "agent": "build",
                "model": {"providerID": "fixture", "id": "actual-model"},
                "tokens": {"input": 5, "output": 2}, "cost": 0.01,
                **({"error": {"message": "fixture failed"}} if outcome == "failed" else {}),
            })
            if emit:
                self.event(session, "session.execution." + outcome,
                           **({"error": {"message": "fixture execution failed"}} if outcome == "failed" else {}))


def handler(backend):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_):
            pass

        def reply(self, value=None, status=200):
            body = b"" if value is None else json.dumps(value).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def do_GET(self):
            self.route()

        def do_POST(self):
            self.route()

        def route(self):
            expected = "Basic " + base64.b64encode(b"opencode:secret").decode()
            if self.headers.get("Authorization") != expected:
                self.reply({"error": "unauthorized"}, 401)
                return
            parsed = urlparse(self.path)
            path, query = parsed.path, parse_qs(parsed.query)
            body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
            if path == "/api/event":
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                stream = queue.Queue()
                backend.streams.append(stream)
                try:
                    while True:
                        try:
                            event = stream.get(timeout=0.2)
                        except queue.Empty:
                            event = "heartbeat"
                        if event is None:
                            self.close_connection = True
                            return
                        payload = b": heartbeat\n\n" if event == "heartbeat" else b"data: " + json.dumps(event).encode() + b"\n\n"
                        self.wfile.write(payload)
                        self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    return
                finally:
                    backend.streams.remove(stream)
            if path == "/api/info":
                return self.reply({"version": "2.1.4"})
            if path == "/api/agent":
                return self.reply({"data": [{"id": "build"}, {"id": "hidden", "hidden": True}]})
            if path == "/api/model":
                return self.reply({"data": [
                    {"id": "model", "providerID": "fixture", "name": "Fixture", "enabled": True,
                     "status": "active", "variants": [{"id": "high"}]},
                    {"id": "off", "providerID": "fixture", "name": "Off", "enabled": False, "status": "active"},
                ]})
            if path == "/api/location":
                directory = query["location[directory]"][0]
                return self.reply({"directory": directory, "project": {"id": "proj_fixture", "canonical": directory}})
            if path == "/api/worktree":
                directory = backend.directory / "worktrees" / body["name"]
                assert "branch" not in body, "branch selects an existing ref in OpenCode"
                subprocess.run(["git", "-C", body["from"], "worktree", "add", "--detach", str(directory)],
                               check=True, capture_output=True)
                return self.reply({"directory": str(directory)})
            if path == "/api/session/active":
                return self.reply({"data": {sid: {"type": "running"} for sid, session in backend.sessions.items()
                                            if session["active"] and not session["pending"]}})
            if path == "/api/session":
                if self.command == "GET":
                    return self.reply({"data": list(backend.sessions.values())[:1]})
                with backend.lock:
                    sid = f"ses_{len(backend.sessions) + 1}"
                    session = {**body, "id": sid, "active": False, "pending": [], "messages": []}
                    backend.sessions[sid] = session
                return self.reply({"data": session})
            parts = path.split("/")
            if len(parts) >= 4 and parts[3] in backend.sessions:
                session = backend.sessions[parts[3]]
                if len(parts) == 4:
                    return self.reply({"data": session})
                action = parts[4]
                if action in ("agent", "model"):
                    session.update(body)
                    return self.reply(status=204)
                if action == "prompt":
                    if body["text"] == "not_accepted":
                        self.close_connection = True
                        return
                    if body["text"] == "rejected":
                        return self.reply({"error": "prompt rejected"}, 400)
                    message = {"id": f"msg_u{len(session['messages'])}", "type": "user", "text": body["text"]}
                    session["messages"].append(message)
                    if body.get("delivery"):
                        session["delivery"] = body["delivery"]
                        return self.reply({"data": message})
                    task = body["text"]
                    session["task"] = task
                    session["active"] = True
                    session.pop("outcome", None)
                    if task == "admission_crash":
                        time.sleep(1)
                    if task == "lost_response":
                        backend.complete(session, emit=False)
                        self.close_connection = True
                        return
                    self.reply({"data": message})
                    backend.event(session, "session.execution.started")
                    backend.event(session, "session.text.delta", delta="working héllo")
                    if task in ("permission", "denied"):
                        request = {"id": "per_" + session["id"], "sessionID": session["id"],
                                   "action": "shell" if task == "denied" else "edit",
                                   "resources": ["git push origin main" if task == "denied" else "file.txt"]}
                        session["pending"] = [request]
                        backend.event(session, "permission.asked", **{k: v for k, v in request.items() if k != "sessionID"})
                    elif task not in ("running", "queued"):
                        backend.complete(session, "failed" if task == "fail" else "succeeded", emit=task not in ("silent", "large"))
                        if task == "large":
                            session["messages"][-1]["content"] = [
                                {"type": "text", "text": "é" * 30_000},
                                *[{"type": "tool", "name": "shell", "state": {"input": {"command": "x" * 20_000}}} for _ in range(40)],
                            ]
                            backend.event(session, "session.execution.succeeded")
                    return
                if action == "message":
                    if session.get("task") == "slow":
                        time.sleep(2)
                    order = query.get("order", ["asc"])[0]
                    start = 0
                    if "cursor" in query:
                        order, offset = query["cursor"][0].split(":")
                        start = int(offset)
                    messages = session["messages"][::1 if order == "asc" else -1]
                    if "type" in query:
                        messages = [message for message in messages if message["type"] == query["type"][0]]
                    end = start + min(2, int(query.get("limit", [2])[0]))
                    return self.reply({"data": messages[start:end], "cursor": {
                        "next": f"{order}:{end}" if end < len(messages) else None}})
                if action == "diff":
                    backend.diffs.append((session["id"], query))
                    return self.reply({"data": [{"file": "file.txt", "patch": "@@\n+done\n", "additions": 1, "deletions": 0, "status": "added"}]})
                if action == "permission":
                    if self.command == "GET":
                        return self.reply({"data": session["pending"]})
                    assert set(body) <= {"decision", "message"}, body
                    if body["decision"] == "reject" and backend.reject_failures:
                        backend.reject_failures -= 1
                        return self.reply({"error": "temporary failure"}, 503)
                    backend.replies.append(body)
                    backend.complete(session, "failed" if body["decision"] == "reject" else "succeeded")
                    return self.reply(status=204)
                if action == "interrupt":
                    assert query == {"resume": ["false"]}, query
                    backend.complete(session, "interrupted")
                    return self.reply(status=204)
            self.reply({"error": f"unexpected endpoint {path}"}, 404)

    return Handler


def eventually(mcp, job, status, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        snapshot = mcp.tool("wait", {"job_id": job, "wait_ms": 100})
        if snapshot["status"] == status:
            return snapshot
        time.sleep(0.02)
    raise AssertionError(f"job never reached {status}: {snapshot}")


def run():
    with tempfile.TemporaryDirectory(prefix="gate-smoke-") as temporary:
        directory = Path(temporary)
        repo = directory / "repo"
        repo.mkdir()
        subprocess.run(["git", "init", "--quiet", str(repo)], check=True)
        (repo / "tracked.txt").write_text("initial\n")
        subprocess.run(["git", "-C", str(repo), "add", "tracked.txt"], check=True)
        subprocess.run(["git", "-C", str(repo), "-c", "user.name=Gate Test", "-c", "user.email=gate@example.invalid",
                        "commit", "--quiet", "-m", "fixture"], check=True)
        backend = Backend(directory)
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler(backend))
        server.daemon_threads = True
        threading.Thread(target=server.serve_forever, daemon=True).start()
        env = {"GATE_OPENCODE_URL": f"http://127.0.0.1:{server.server_port}", "GATE_OPENCODE_PASSWORD": "secret",
               "GATE_DIRECTORY": str(repo), "GATE_PERMISSION_MODE": "ask", "GATE_DENY_SHELL": "git push",
               "GATE_STATE_DIR": str(directory / "jobs"), "GATE_ARTIFACT_DIR": str(directory / "artifacts")}
        try:
            doctor = subprocess.run([str(BINARY), "doctor", "--json"], env={**os.environ, **env}, capture_output=True, text=True)
            assert doctor.returncode == 0, doctor.stderr
            assert json.loads(doctor.stdout)["ok"]
            assert "secret" not in doctor.stdout
            with Mcp(env) as mcp:
                tools = mcp.rpc("tools/list")["tools"]
                assert {tool["name"] for tool in tools} == {
                    "delegate", "wait", "status", "messages", "steer", "cancel", "diff", "reply_permission", "list", "agents", "models"}
                assert len(mcp.tool("agents")["agents"]) == 1
                assert mcp.tool("models")["models"][0]["model"] == "fixture/model"
                assert len(mcp.tool("models")["models"]) == 1
                assert "empty" in mcp.tool("delegate", {"task": " "}, error=True)
                print("ok: discovery, MCP schemas, doctor, validation", flush=True)

                first = mcp.tool("delegate", {"task": "complete", "wait_ms": 0})
                done = eventually(mcp, first["job_id"], "completed")
                assert done["summary"] == "done" and done["model"] == "fixture/actual-model"
                artifact = json.loads(Path(done["artifact"]).read_text())
                assert len(artifact["messages"]) == 2 and artifact["diff"][0]["patch"].endswith("+done\n")
                assert "different sessions" in mcp.tool("diff", {"job_id": done["job_id"], "session_id": "ses_other"}, error=True)
                continued = mcp.tool("delegate", {"task": "complete", "session_id": done["session_id"], "wait_ms": 0})
                continued = eventually(mcp, continued["job_id"], "completed")
                assert continued["usage"]["input"] == 5
                mcp.tool("diff", {"job_id": done["job_id"], "context": 3})
                assert backend.diffs[-1][1]["from"] == [done["first_message_id"]]
                assert len(json.loads(Path(continued["artifact"]).read_text())["messages"]) == 2
                print("ok: task completion, full artifacts, session reuse, pinned diffs", flush=True)

                rejected = mcp.tool("delegate", {"task": "rejected", "wait_ms": 0})
                assert rejected["status"] == "failed" and "400" in rejected["error"]
                assert Path(rejected["artifact"]).is_file()
                failed = mcp.tool("delegate", {"task": "fail", "wait_ms": 0})
                failed = eventually(mcp, failed["job_id"], "failed")
                assert "fixture" in failed["error"]
                lost = mcp.tool("delegate", {"task": "lost_response", "wait_ms": 0})
                eventually(mcp, lost["job_id"], "completed")
                unknown = mcp.tool("delegate", {"task": "not_accepted", "session_id": continued["session_id"], "wait_ms": 0})
                assert unknown["status"] == "running" and not unknown["prompt_admitted"]
                assert "cancel" in unknown["next"] and "could not be confirmed" in unknown["error"]
                assert "uncertain" in mcp.tool("steer", {"job_id": unknown["job_id"], "text": "retry"}, error=True)
                assert mcp.tool("cancel", {"job_id": unknown["job_id"]})["status"] == "cancelled"
                large = mcp.tool("delegate", {"task": "large", "wait_ms": 0})
                large = eventually(mcp, large["job_id"], "completed")
                assert len(large["summary"]) < 8_100
                history = mcp.tool("messages", {"job_id": large["job_id"], "max_chars": 1000})
                assert len(json.dumps(history, ensure_ascii=False)) < 2_000
                assert len(json.loads(Path(large["artifact"]).read_text())["summary"]) == 30_000
                old_history = mcp.tool("messages", {"job_id": done["job_id"]})
                assert old_history["messages"][0]["id"] == done["first_message_id"]
                print("ok: rejected admission artifact, lost response recovery, output clipping, historical messages", flush=True)

                slow = mcp.tool("delegate", {"task": "slow", "wait_ms": 0})
                blocked = mcp.tool("delegate", {"task": "permission", "wait_ms": 500})
                blocked = eventually(mcp, blocked["job_id"], "blocked_on_permission", timeout=1.5)
                assert mcp.tool("reply_permission", {"job_id": blocked["job_id"], "reply": "once"})["status"] in ("running", "completed")
                eventually(mcp, blocked["job_id"], "completed")
                eventually(mcp, slow["job_id"], "completed")
                print("ok: permissions remain responsive while another job finalizes", flush=True)

                backend.reject_failures = 1
                denied = mcp.tool("delegate", {"task": "denied", "wait_ms": 300})
                denied = eventually(mcp, denied["job_id"], "blocked_on_permission")
                assert "denied prefix" in mcp.tool("reply_permission", {"job_id": denied["job_id"], "reply": "always"}, error=True)
                eventually(mcp, denied["job_id"], "failed")
                assert backend.replies[-1]["decision"] == "reject"
                print("ok: deny policy, failed rejection retry, manual override protection", flush=True)

                isolated = mcp.tool("delegate", {"task": "complete", "worktree": True, "wait_ms": 0})
                isolated = eventually(mcp, isolated["job_id"], "completed")
                assert isolated["directory"] != str(repo) and isolated["git"]
                assert (Path(isolated["directory"]) / "tracked.txt").read_text() == "initial\n"
                assert isolated["worktree"]["branch"] is None
                print("ok: isolated worktree with committed files and detached HEAD", flush=True)

                queued = mcp.tool("delegate", {"task": "queued", "wait_ms": 0})
                assert "active" in mcp.tool("delegate", {"task": "complete", "session_id": queued["session_id"]}, error=True)
                mcp.tool("steer", {"job_id": queued["job_id"], "text": "next", "delivery": "queue"})
                backend.complete(backend.sessions[queued["session_id"]], active=True)
                time.sleep(0.1)
                assert mcp.tool("status", {"job_id": queued["job_id"]})["status"] == "running"
                backend.complete(backend.sessions[queued["session_id"]])
                eventually(mcp, queued["job_id"], "completed")
                print("ok: queue stays running through intermediate turn completion", flush=True)

                # A reviewer on another Gate process continues the orchestrator's finished session.
                built = mcp.tool("delegate", {"task": "complete", "wait_ms": 0})
                built = eventually(mcp, built["job_id"], "completed")
                assert built["client"] == "gate-smoke"
                # Its own state directory keeps its records out of the recovery checks below.
                with Mcp({**env, "GATE_STATE_DIR": str(directory / "reviewer-jobs")}) as reviewer:
                    fix = reviewer.tool("delegate", {"task": "running", "session_id": built["session_id"], "wait_ms": 0})
                    assert fix["job_id"] != built["job_id"] and fix["status"] == "running"
                    mcp.tool("diff", {"session_id": built["session_id"], "include_patch": False})
                    assert "to" not in backend.diffs[-1][1], backend.diffs[-1]
                    mcp.tool("diff", {"job_id": built["job_id"], "include_patch": False})
                    assert "to" in backend.diffs[-1][1], backend.diffs[-1]
                    stopped = mcp.tool("cancel", {"session_id": built["session_id"]})
                    assert "interrupted" in stopped and stopped["status"] == "completed"
                    assert backend.sessions[built["session_id"]]["outcome"] == "interrupted"
                    eventually(reviewer, fix["job_id"], "cancelled")
                print("ok: orchestrator diff and cancel by session cover a reviewer's later turn", flush=True)

                silent = mcp.tool("delegate", {"task": "silent", "wait_ms": 0})
                eventually(mcp, silent["job_id"], "completed")
                running = mcp.tool("delegate", {"task": "running", "wait_ms": 200})
                session = backend.sessions[running["session_id"]]
                foreign = {**session, "location": {"directory": "/foreign"}}
                backend.event(foreign, "session.text.delta", delta="FOREIGN")
                time.sleep(0.05)
                assert "FOREIGN" not in json.dumps(mcp.tool("status", {"job_id": running["job_id"]}))
                for stream in list(backend.streams):
                    stream.put(None)
                backend.complete(session, emit=False)
                eventually(mcp, running["job_id"], "completed")
                print("ok: missed completion, dropped SSE connection, location filtering", flush=True)

                missing = mcp.tool("delegate", {"task": "running", "wait_ms": 0})
                del backend.sessions[missing["session_id"]]
                missing = eventually(mcp, missing["job_id"], "failed")
                assert "no longer exists" in missing["error"]

                pending = mcp.tool("delegate", {"task": "permission", "wait_ms": 200})
                pending = eventually(mcp, pending["job_id"], "blocked_on_permission")
                with Mcp(env) as other:
                    assert other.tool("list")["count"] == 0
                before = pending["job_id"]
            with Mcp(env) as recovered:
                assert before in {job["job_id"] for job in recovered.tool("list")["jobs"]}
                pending = eventually(recovered, before, "blocked_on_permission")
                recovered.tool("reply_permission", {"job_id": before, "reply": "once"})
                finished = eventually(recovered, before, "completed")
                frozen = recovered.tool("status", {"job_id": finished["job_id"]})["elapsed_ms"]
                assert frozen == finished["elapsed_ms"]
                running = recovered.tool("delegate", {"task": "running", "wait_ms": 0})
                cancelled = recovered.tool("cancel", {"job_id": running["job_id"]})
                assert cancelled["status"] == "cancelled" and Path(cancelled["artifact"]).is_file()
                assert "finished" in recovered.tool("steer", {"job_id": running["job_id"], "text": "again"}, error=True)
            print("ok: restart recovery, process ownership, stable durations, cancellation artifact", flush=True)

            storage_file = directory / "not-a-directory"
            storage_file.write_text("fixture")
            with Mcp({**env, "GATE_ARTIFACT_DIR": str(storage_file)}) as broken_storage:
                job = broken_storage.tool("delegate", {"task": "complete", "wait_ms": 0})
                job = eventually(broken_storage, job["job_id"], "completed")
                assert job["artifact"] is None and "could not write artifact" in job["error"]
            print("ok: artifact write failures are reported", flush=True)

            # Kill Gate after the backend accepts a prompt but before it replies.
            with Mcp(env) as interrupted:
                previous = set(backend.sessions)
                interrupted.counter += 1
                interrupted.send({"jsonrpc": "2.0", "id": interrupted.counter, "method": "tools/call",
                                  "params": {"name": "delegate", "arguments": {"task": "admission_crash", "wait_ms": 0}}})
                deadline = time.monotonic() + 5
                while set(backend.sessions) == previous and time.monotonic() < deadline:
                    time.sleep(0.01)
                created = list(set(backend.sessions) - previous)
                assert len(created) == 1
                sid = created[0]
                records = [json.loads(path.read_text()) for path in (directory / "jobs").glob("*.json")]
                uncertain = next(record["job"] for record in records if record["job"]["session_id"] == sid)
                interrupted.process.kill()
                interrupted.process.wait()
            time.sleep(1.1)
            with Mcp(env) as restored:
                uncertain = eventually(restored, uncertain["job_id"], "failed")
                assert "before prompt admission was confirmed" in uncertain["error"]
                assert len(backend.sessions[sid]["messages"]) == 2
            print("ok: interrupted admission is surfaced without replaying the task", flush=True)

            bad_auth = subprocess.run([str(BINARY), "doctor", "--json"], env={**os.environ, **env, "GATE_OPENCODE_PASSWORD": "wrong"}, capture_output=True, text=True)
            assert bad_auth.returncode != 0 and not json.loads(bad_auth.stdout)["ok"]
            invalid_config = subprocess.run([str(BINARY), "doctor", "--json"], env={**os.environ, **env, "GATE_PERMISSION_MODE": "typo"}, capture_output=True, text=True)
            assert invalid_config.returncode != 0 and not json.loads(invalid_config.stdout)["ok"]
            print("ok: doctor reports authentication failure", flush=True)
        finally:
            for stream in list(backend.streams):
                stream.put(None)
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    run()
