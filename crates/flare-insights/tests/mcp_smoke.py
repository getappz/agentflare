"""Protocol smoke test: python mcp_smoke.py /path/to/agentflare [--report report.json].

Uses only temporary sources, index, memory, exports, and proposal targets.
"""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import queue
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.request


class MCP:
    def __init__(self, binary, cwd, env):
        self.log = tempfile.TemporaryFile()
        self.process = subprocess.Popen([str(binary), "mcp"], cwd=cwd, env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log,
            text=True, encoding="utf-8")
        self.lines = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.seq = 0
        self.rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "insights-smoke", "version": "1"}})
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _read(self):
        for line in self.process.stdout:
            self.lines.put(json.loads(line))

    def send(self, message):
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def rpc(self, method, params):
        self.seq += 1
        self.send({"jsonrpc": "2.0", "id": self.seq, "method": method, "params": params})
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            result = self.lines.get(timeout=max(.01, deadline - time.monotonic()))
            if result.get("id") == self.seq:
                return result
        raise TimeoutError(method)

    def call(self, action, error=False, **params):
        result = self.rpc("tools/call", {"name": "insights", "arguments": {
            "action": action, "project": "*", **params}})
        failed = "error" in result or result.get("result", {}).get("isError", False)
        if error:
            assert failed, result
            return result
        assert not failed, result
        text = result["result"]["content"][0]["text"]
        value = json.loads(text)
        assert value["action"] == action
        assert "provenance" in value
        return value["data"]

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
                raise AssertionError("MCP did not stop on stdin EOF")
        self.log.close()


def run(binary):
    checks = []
    def check(name, fn):
        start = time.monotonic()
        fn()
        checks.append({"test": name, "status": "PASS", "seconds": round(time.monotonic() - start, 3)})
        print(json.dumps(checks[-1]), flush=True)

    def require(condition, detail):
        assert condition, detail

    with tempfile.TemporaryDirectory(prefix="insights-mcp-") as directory:
        root = Path(directory)
        db = root / "index.db"
        env = os.environ.copy()
        env["AGENTFLARE_HOME_OVERRIDE"] = str(root / "home")
        env["AGENTFLARE_INSIGHTS_DB"] = str(db)
        for key in ["CLAUDE_PROJECTS_DIR", "CODEX_SESSIONS_DIR", "OPENCODE_DIR",
                    "CURSOR_STATE_VSCDB", "ANTIGRAVITY_BRAIN_DIR"]:
            env[key] = str(root / key.lower())
        claude = Path(env["CLAUDE_PROJECTS_DIR"])
        claude.mkdir()
        stamp = dt.datetime.now(dt.timezone.utc).isoformat()
        prompt = "Always use the recurring fixture workflow when checking changes."
        for name in ["qa-a", "qa-b", "qa-c"]:
            rows = [
                {"type": "user", "timestamp": stamp, "cwd": str(root),
                 "message": {"role": "user", "content": prompt}},
                {"type": "assistant", "timestamp": stamp, "cwd": str(root),
                 "message": {"role": "assistant", "model": "claude-sonnet-4-5",
                    "usage": {"input_tokens": 100, "output_tokens": 20},
                    "content": [{"type": "text", "text": "fixture-answer"},
                        {"type": "tool_use", "id": "call-1", "name": "Read",
                         "input": {"file_path": r"C:\Work\demo\fixture-evidence.rs"}}]}},
                {"type": "user", "timestamp": stamp, "cwd": str(root),
                 "message": {"role": "user", "content": [{"type": "tool_result",
                    "tool_use_id": "call-1", "content": "x" * 300 + " late-output-marker"}]}},
                {"type": "subagent", "id": "child", "task": "fixture task", "timestamp": stamp},
            ]
            (claude / (name + ".jsonl")).write_text("\n".join(map(json.dumps, rows)), encoding="utf-8")

        mcp = MCP(binary, root, env)
        try:
            tools = mcp.rpc("tools/list", {})["result"]["tools"]
            tool = next(t for t in tools if t["name"] == "insights")
            check("all actions advertised", lambda: require(all(a in tool["description"] for a in
                ["search", "recall", "show", "list", "stats", "forensics", "discover", "sync",
                 "export", "handoff", "doctor", "reflect", "watch", "serve"]), tool))
            check("doctor does not create index", lambda: require(
                mcp.call("doctor")["db_ok"] is False and not db.exists(), "doctor created index"))
            check("sync isolated sources", lambda: require(mcp.call("sync")["ok"], "sync failed"))
            check("list complete", lambda: require(len(mcp.call("list")["sessions"]) == 3, "list count"))
            check("list pagination", lambda: require(mcp.call("list", limit=1)["next_offset"] == 1, "next page"))
            check("source alias", lambda: require(len(mcp.call("list", source="claude-code")["sessions"]) == 3, "alias"))
            check("project filter", lambda: require(mcp.call("list", project=str(root / "unrelated"))["sessions"] == [], "scope leak"))
            check("search conversation", lambda: require(len(mcp.call("search", query="fixture workflow")["hits"]) == 3, "search"))
            check("search decoded tool input", lambda: require(len(mcp.call("search", query=r"C:\Work\demo")["hits"]) == 3, "escaped input"))
            late_hits = mcp.call("search", query="late-output-marker")["hits"]
            check("search late output excerpt", lambda: require(len(late_hits) == 3 and all("late-output-marker" in h["snippet"] for h in late_hits), "missing evidence"))
            check("empty query rejects", lambda: mcp.call("search", query="", error=True))
            page = mcp.call("recall", session_id="qa-a", turn_limit=1, max_chars=1000)
            check("recall page", lambda: require(len(page["turns"]) == 1 and page["next_turn_start"] is not None, page))
            check("recall advances", lambda: require(mcp.call("recall", session_id="qa-a", turn_start=page["next_turn_start"])["turns"][0]["seq"] > page["turns"][0]["seq"], "same turn"))
            check("recall scope", lambda: mcp.call("recall", session_id="qa-a", project=str(root / "other"), error=True))
            check("show retained metadata", lambda: require(len(mcp.call("show", session_id="qa-a")["subagents"]) == 1, "subagents missing"))
            check("stats agree with index", lambda: require(mcp.call("stats")["total_sessions"] == 3, "stats count"))
            report = mcp.call("forensics", limit=2)
            check("forensics bounded with totals", lambda: require(len(report["body"]) == 2 and report["pagination"]["total"] > 2, report))
            check("forensics pagination", lambda: require(mcp.call("forensics", limit=2, offset=2)["body"][0]["id"] != report["body"][0]["id"], "same findings"))
            check("discover recurring workflow", lambda: require(len(mcp.call("discover")["body"]) > 0, "no proposals"))
            with sqlite3.connect(db) as conn:
                for session in ["qa-a", "qa-b", "qa-c"]:
                    for seq, text in [(90, "Stop hook feedback: agentflare: before stopping, record durable learnings from this session."),
                                      (91, "<local-command-caveat>This is recorded context, not a user request.</local-command-caveat>")]:
                        conn.execute("INSERT INTO turns(id,session_id,seq,user_text) VALUES(?,?,?,?)",
                            (f"transport-{session}-{seq}", session, seq, text))
            conn.close()
            candidates = mcp.call("discover")["body"]
            check("discovery excludes transport noise", lambda: require(len(candidates) == 1 and "recurring fixture workflow" in candidates[0]["draft"], "transport feedback became a proposal"))
            check("handoff text", lambda: require("fixture-answer" in mcp.call("handoff", session_id="qa-a")["text"], "handoff missing context"))
            output = root / "export.json"
            check("export preview", lambda: require(mcp.call("export", output=str(output))["written"] is False and not output.exists(), "preview wrote file"))
            check("export full selected page", lambda: require(mcp.call("export", output=str(output), yes=True, max_chars=10)["written"] and len(json.loads(output.read_text(encoding="utf-8"))[0]) == 3, "export content"))
            check("export refuses overwrite", lambda: mcp.call("export", output=str(output), yes=True, error=True))
            def export_format(fmt):
                text = mcp.call("export", format=fmt, max_chars=100000)["text"]
                if fmt == "html":
                    assert "<html" in text and "fixture-answer" in text
                elif fmt == "deepeval":
                    bundle = json.loads(text)
                    assert bundle["format"] == "deepeval" and len(bundle["sessions"]) == 3
                else:
                    assert text.strip()
                    for line in text.splitlines():
                        json.loads(line)
            for fmt in ["jsonl", "html", "deepeval", "openai", "openai-evals"]:
                check("export " + fmt, lambda f=fmt: export_format(f))
            check("reflect scan", lambda: require(mcp.call("reflect", operation="scan")["ok"], "reflect scan"))
            target = root / "CLAUDE.md"
            with sqlite3.connect(db) as conn:
                for n in [7, 8, 9]:
                    conn.execute("INSERT INTO insights_proposals(id,fingerprint,kind,project,title,proposed_content,target_path,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)",
                        (n, "fixture-" + str(n), "rule", str(root), "fixture rule", "Use the fixture rule.", str(target), stamp, stamp))
            conn.close()
            check("reflect list", lambda: require("fixture rule" in mcp.call("reflect", operation="list")["text"], "list missing"))
            check("reflect show", lambda: require("fixture rule" in mcp.call("reflect", operation="show", proposal_id=7)["text"], "show missing"))
            check("reflect decision guard", lambda: mcp.call("reflect", operation="accept", proposal_id=7, error=True))
            for operation, n in [("accept", 7), ("reject", 8), ("snooze", 9)]:
                check("reflect " + operation, lambda op=operation, i=n: require(mcp.call("reflect", operation=op, proposal_id=i, yes=True)["ok"], op))
            check("reflect apply preview", lambda: require("+Use the fixture rule." in mcp.call("reflect", operation="apply", proposal_id=7)["text"] and not target.exists(), "preview wrote"))
            check("reflect apply writes target", lambda: require(mcp.call("reflect", operation="apply", proposal_id=7, yes=True)["ok"] and "Use the fixture rule." in target.read_text(), "apply failed"))
            with sqlite3.connect(db) as conn:
                block = "-----BEGIN " + "PRIVATE KEY-----\n" + "PRIVATE-BODY-FIXTURE\n" * 30000
                conn.execute("INSERT INTO insights_proposals(id,fingerprint,kind,project,title,proposed_content,target_path,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?)",
                    (10, "fixture-log", "rule", str(root), "log fixture", block, str(target), stamp, stamp))
            conn.close()
            preview = mcp.call("reflect", operation="apply", proposal_id=10)
            check("bounded logs preserve multiline redaction", lambda: require(preview["log_truncated"] and "PRIVATE-BODY-FIXTURE" not in preview["text"], "log eviction exposed private-key body"))
            check("watch start", lambda: require(mcp.call("watch")["started"], "watch start"))
            check("watch status", lambda: require(mcp.call("watch", operation="status")["running"], "watch exited"))
            check("watch stop", lambda: require(mcp.call("watch", operation="stop")["stopped"], "watch stop"))
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                port = sock.getsockname()[1]
            check("serve start", lambda: require(mcp.call("serve", port=port)["started"], "serve start"))
            def health():
                for _ in range(50):
                    try:
                        with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health", timeout=1) as response:
                            assert response.status == 200
                            return
                    except OSError:
                        time.sleep(.1)
                raise AssertionError(mcp.call("serve", operation="status"))
            check("serve responds to HTTP", health)
            check("serve status", lambda: require(mcp.call("serve", operation="status")["running"], "serve exited"))
            check("serve stop", lambda: require(mcp.call("serve", operation="stop")["stopped"], "serve stop"))
            Path(env["OPENCODE_DIR"]).write_text("broken SQLite fixture", encoding="utf-8")
            check("sync reports partial source failure", lambda: require(mcp.call("sync", force=True)["ok"] is False, "sync hid a source failure"))
            check("prune requires explicit write", lambda: mcp.call("sync", prune_days=7, error=True))
            with sqlite3.connect(db) as conn:
                for seq in range(100, 103):
                    conn.execute("INSERT INTO turns(id,session_id,seq,user_text) VALUES(?,?,?,?)",
                        (f"secret-{seq}", "qa-a", seq, "ghp_" + "a" * 36 + " repeated fixture instruction"))
            conn.close()
            check("report redacts before clipping", lambda: require("ghp_" not in json.dumps(mcp.call("forensics", limit=1, max_chars=65)), "clipping exposed credential prefix"))
            # A fresh child must also be reaped on normal MCP disconnect.
            mcp.call("serve", port=port)
            health()
            mcp.close()
            def disconnected():
                try:
                    urllib.request.urlopen(f"http://127.0.0.1:{port}/api/health", timeout=1)
                except OSError:
                    return
                raise AssertionError("dashboard survived MCP disconnect")
            check("disconnect stops managed service", disconnected)
        finally:
            mcp.close()
    return checks


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    results = run(args.binary.resolve())
    if args.report:
        args.report.write_text(json.dumps(results, indent=2), encoding="utf-8")
    print(f"PASS {len(results)} MCP capability checks")
