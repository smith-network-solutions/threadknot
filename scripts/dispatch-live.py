#!/usr/bin/env python3
"""Dispatch with a REAL agent doing the work.

`dispatch-smoke.py` proves the plumbing with fabricated reports. This one proves
the thing the feature is actually for: a planning thread hands a brief to a
DIFFERENT harness, that harness runs, does the work in its own thread, files a
structured report, and the planner reads it.

Needs a real HOME (so the agent CLIs are on PATH and logged in) and its own data
dir + port, so it never touches the live install's threads. It spends real
subscription tokens — the briefs are deliberately tiny.

Usage: python3 scripts/dispatch-live.py [agent]     # default: codex
"""

import json
import os
import shutil
import subprocess
import sys
import time
from websockets.sync.client import connect

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "src-tauri/target/debug/threadknot-headless")
PORT = 22930
BASE = "/tmp/tk-live"
AGENT = sys.argv[1] if len(sys.argv) > 1 else "codex"
failures = []


def check(ok, label):
    print(("  PASS  " if ok else "  FAIL  ") + label)
    if not ok:
        failures.append(label)


def rpc(token, kind, payload, rid=1, timeout=120):
    with connect(f"ws://127.0.0.1:{PORT}/ws?token={token}", open_timeout=30,
                 max_size=64 * 1024 * 1024) as ws:
        ws.send(json.dumps({"id": rid, "type": kind, "payload": payload}))
        while True:
            msg = json.loads(ws.recv(timeout=timeout))
            if msg.get("type") == "response" and msg.get("id") == rid:
                return msg


def ok(token, kind, payload, **kw):
    r = rpc(token, kind, payload, **kw)
    if not r.get("ok"):
        raise AssertionError(f"{kind} failed: {r.get('error')}")
    return r.get("data", {})


def main():
    shutil.rmtree(BASE, ignore_errors=True)
    os.makedirs(f"{BASE}/data")
    os.makedirs(f"{BASE}/work")
    # Real HOME on purpose: the agent CLIs' logins live there. Nothing here
    # installs skills or MCP servers, which is the one thing that would write
    # into the live CLI config.
    env = dict(os.environ, THREADKNOT_DATA_DIR=f"{BASE}/data",
               THREADKNOT_PORT=str(PORT))
    log = open("/tmp/tk-live.log", "w")
    proc = subprocess.Popen([BIN], env=env, stdout=log, stderr=log)
    try:
        token = None
        for _ in range(80):
            try:
                token = json.load(open(f"{BASE}/data/server.json"))["token"]
                rpc(token, "hello", {})
                break
            except Exception:
                time.sleep(0.5)
        if not token:
            raise SystemExit("server never came up; see /tmp/tk-live.log")

        info = ok(token, "hello", {})
        print(f"  agents available: {[a.get('agent') for a in info.get('agents', [])]}")

        project = ok(token, "project.create", {"path": f"{BASE}/work"})["id"]
        parent = ok(token, "thread.create", {
            "projectId": project, "agent": "claude",
            "settings": {"model": "claude-opus-5", "access": "full", "mode": "build",
                         "wideContext": False, "claudeChrome": False},
        })["id"]

        print(f"\n== dispatching real work to {AGENT} ==")
        made = ok(token, "dispatch.create", {
            "parentThreadId": parent,
            "machineId": info["machineId"],
            "projectId": project,
            "agent": AGENT,
            "label": f"probe:{AGENT}",
            "brief": (
                "Create a file named dispatched.txt in the current directory whose only "
                "contents are the word: banana\n"
                "Then call report_result with status succeeded and a one-sentence summary. "
                "Do nothing else."
            ),
        })
        dispatch_id = made["id"]
        child = made["childThreadId"]
        check(bool(child), "a worker thread was created")
        check(made["settings"]["access"] == "full", "it inherited the parent's access")
        check(made["agent"] == AGENT, f"it runs on {AGENT}, not the parent's agent")

        print("  waiting for the worker to finish (up to 5 min)…")
        deadline = time.time() + 300
        record = None
        while time.time() < deadline:
            record = ok(token, "dispatch.get", {"dispatchId": dispatch_id})
            if record["status"] not in ("queued", "running"):
                break
            time.sleep(3)

        status = record["status"] if record else "?"
        print(f"  final status: {status}")
        check(status == "succeeded", f"the dispatch succeeded (was {status})")

        result = (record or {}).get("result") or {}
        check(bool(result.get("summary")), "a summary came back")
        if result.get("summary"):
            print(f"  summary: {result['summary'][:300]}")
        check(not result.get("inferred"),
              "the worker filed a STRUCTURED report (report_result), not an inferred one")

        produced = os.path.join(BASE, "work", "dispatched.txt")
        check(os.path.exists(produced), "the worker actually did the work on disk")
        if os.path.exists(produced):
            body = open(produced).read().strip()
            check("banana" in body, f"the file has the right contents ({body[:40]!r})")

        # The worker's reasoning must remain inspectable, not collapsed to a summary.
        events = ok(token, "thread.get", {"threadId": child})
        check(bool(events), "the worker's own thread is still readable")

        print("\n== the parent's feed shows the worker ==")
        parent_events = ok(token, "thread.get", {"threadId": parent})
        blob = json.dumps(parent_events)
        check("subagent_started" in blob, "a worker row was announced to the parent")
        check("subagent_completed" in blob, "its completion was announced to the parent")

    finally:
        proc.terminate()
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()

    print()
    if failures:
        print(f"{len(failures)} FAILED:")
        for f in failures:
            print(f"  - {f}")
        sys.exit(1)
    print("all checks passed")


if __name__ == "__main__":
    main()
