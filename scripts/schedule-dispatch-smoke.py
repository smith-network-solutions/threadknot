#!/usr/bin/env python3
"""Phase 4: a SCHEDULE that dispatches, proven end to end across two machines.

`dispatch-smoke.py` proves an agent can delegate. This proves the other half of
Phase 4 — that the same delegation happens on a timer with nobody driving it —
and it asserts the properties that make an unattended fan-out trustworthy rather
than merely working:

  * a schedule can carry a dispatch block, and it survives the round trip;
  * firing one opens a COORDINATOR thread that runs no turn of its own and
    creates one dispatch per named machine, including one on the other machine;
  * the coordinator's transcript carries the brief and a `subagent_started` per
    worker, so the crew is inspectable rather than implied;
  * a target that cannot be reached is REPORTED, not skipped — the run says how
    many of its workers started and names the ones that refused. A nightly build
    that quietly ran on two machines out of three is worse than one that ran on
    none, because the missing platform gets found by a user;
  * the coordinator SETTLES when its last worker reports. Nothing else can
    settle it: it never ran a turn, so no `turn_completed` is coming;
  * `dispatch` is a second grant on top of `Threads`. A schedule that dispatches
    runs code on other machines unattended, and the capability table can only
    name one capability per request kind.

The workers here have no agent credentials (HOME is sandboxed, as in the other
smoke scripts, because the Library writes into the CLIs' own global skill dirs
and THREADKNOT_DATA_DIR does not isolate those). Their turns therefore FAIL —
which is the point: the failure path is the one that has to deliver a report,
and a test that only ever exercises success would not notice if it didn't.

Usage: python3 scripts/schedule-dispatch-smoke.py [path-to-threadknot-headless]
"""

import json
import os
import shutil
import subprocess
import sys
import time
from websockets.sync.client import connect

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    ROOT, "src-tauri/target/debug/threadknot-headless"
)
# Below `ip_local_port_range`, so an ephemeral outbound socket cannot already
# hold the port (see mesh-smoke.py, where exactly that happened).
PORT_A, PORT_B = 22930, 22940
failures = []
procs = {}


def check(ok_, label):
    print(("  PASS  " if ok_ else "  FAIL  ") + label)
    if not ok_:
        failures.append(label)


def spawn(tag, port):
    base = f"/tmp/tk-schedsp-{tag}"
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(f"{base}/home")
    os.makedirs(f"{base}/data")
    os.makedirs(f"{base}/work")
    env = dict(os.environ, HOME=f"{base}/home",
               THREADKNOT_DATA_DIR=f"{base}/data", THREADKNOT_PORT=str(port))
    log = open(f"/tmp/tk-schedsp-{tag}.log", "w")
    proc = subprocess.Popen([BIN], env=env, stdout=log, stderr=log)
    procs[tag] = (proc, port, base)
    return proc, f"{base}/data", f"{base}/work"


def wait_for_token(data_dir, timeout=30):
    deadline = time.time() + timeout
    path = os.path.join(data_dir, "server.json")
    while time.time() < deadline:
        try:
            return json.load(open(path))["token"]
        except (OSError, KeyError, json.JSONDecodeError):
            time.sleep(0.2)
    raise SystemExit(f"{path} never appeared")


def rpc(port, token, kind, payload, rid=1, timeout=60):
    frame = {"id": rid, "type": kind, "payload": payload}
    with connect(f"ws://127.0.0.1:{port}/ws?token={token}", open_timeout=30,
                 max_size=64 * 1024 * 1024) as ws:
        ws.send(json.dumps(frame))
        while True:
            msg = json.loads(ws.recv(timeout=timeout))
            if msg.get("type") == "response" and msg.get("id") == rid:
                return msg


def ok(port, token, kind, payload, **kw):
    r = rpc(port, token, kind, payload, **kw)
    if not r.get("ok"):
        raise AssertionError(f"{kind} failed: {r.get('error')}")
    return r.get("data", {})


def wait_online(port, token, deadline=40):
    end = time.time() + deadline
    while time.time() < end:
        peers = rpc(port, token, "peer.list", {})["data"]["peers"]
        if any(p.get("online") for p in peers):
            return True
        time.sleep(1)
    return False


def settings(access="full"):
    return {"model": "claude-opus-5", "access": access, "mode": "build",
            "wideContext": False, "claudeChrome": False}


def main():
    _, dir_a, work_a = spawn("a", PORT_A)
    _, dir_b, work_b = spawn("b", PORT_B)
    try:
        token_a = wait_for_token(dir_a)
        token_b = wait_for_token(dir_b)
        for port, token in ((PORT_A, token_a), (PORT_B, token_b)):
            for _ in range(80):
                try:
                    rpc(port, token, "hello", {})
                    break
                except Exception:
                    time.sleep(0.5)

        print("\n== setup ==")
        added = ok(PORT_A, token_a, "peer.add",
                   {"url": f"http://127.0.0.1:{PORT_B}/?token={token_b}"})
        machine_b = added["machineId"]
        machine_a = ok(PORT_A, token_a, "hello", {})["machineId"]
        check(wait_online(PORT_A, token_a), "A and B are paired and online")

        project_a = ok(PORT_A, token_a, "project.create", {"path": work_a})["id"]
        ok(PORT_B, token_b, "project.create", {"path": work_b})
        workspace_id = project_a
        ok(PORT_A, token_a, "workspace.attachRoot",
           {"workspaceId": workspace_id, "machineId": machine_b, "path": work_b})
        members = next(w for w in ok(PORT_A, token_a, "workspace.list", {})["workspaces"]
                       if w["id"] == workspace_id)["members"]
        check(len(members) == 2, "the workspace has a root on each machine")

        # -------------------------------------------------- the record shape ---
        print("\n== a schedule can carry a dispatch block ==")
        sched = ok(PORT_A, token_a, "schedule.create", {
            "projectId": project_a,
            "agent": "claude",
            "settings": settings(),
            "name": "nightly build",
            "prompt": "Produce a release build and report the version.",
            "cadence": {"type": "daily", "time": "03:00"},
            "dispatch": {"machines": [machine_a, machine_b],
                         "syncRef": False},
        })
        check(sched.get("dispatch") is not None, "the block round-trips on create")
        check(sched["dispatch"]["machines"] == [machine_a, machine_b],
              "both targets are stored, in order")

        listed = next(s for s in ok(PORT_A, token_a, "schedule.list", {})["schedules"]
                      if s["id"] == sched["id"])
        check(listed.get("dispatch") is not None, "and survives a reload from disk")

        # An existing plain schedule must not be touched by an update that says
        # nothing about dispatch — absent is not the same as null.
        plain = ok(PORT_A, token_a, "schedule.create", {
            "projectId": project_a, "agent": "claude", "settings": settings(),
            "name": "plain", "prompt": "just run here",
            "cadence": {"type": "daily", "time": "04:00"},
        })
        check(plain.get("dispatch") is None, "a schedule with no block runs here")
        renamed = ok(PORT_A, token_a, "schedule.update",
                     {"scheduleId": sched["id"], "name": "nightly build v2"})
        check(renamed.get("dispatch") is not None,
              "an update that omits `dispatch` leaves the mode alone")
        cleared = ok(PORT_A, token_a, "schedule.update",
                     {"scheduleId": sched["id"], "dispatch": None})
        check(cleared.get("dispatch") is None, "an explicit null turns delegation off")
        ok(PORT_A, token_a, "schedule.update",
           {"scheduleId": sched["id"],
            "dispatch": {"machines": [machine_a, machine_b], "syncRef": False}})

        # ------------------------------------------------------- the firing ---
        print("\n== firing it dispatches to both machines ==")
        fired = ok(PORT_A, token_a, "schedule.run", {"scheduleId": sched["id"]},
                   timeout=120)
        parent = fired["threadId"]

        records = ok(PORT_A, token_a, "dispatch.list", {"threadId": parent})["dispatches"]
        check(len(records) == 2, f"one dispatch per named machine (got {len(records)})")
        machines = {r["machineId"] for r in records}
        check(machines == {machine_a, machine_b},
              "one landed here and one on the other machine")
        check(all(r["parentThreadId"] == parent for r in records),
              "both are parented on the schedule's own thread")
        check(all(r["label"].startswith("nightly build") for r in records),
              "rows are labelled from the schedule, per machine")
        remote = next(r for r in records if r["machineId"] == machine_b)
        check(bool(remote["childThreadId"]),
              "the remote worker's thread was created on B")

        # The coordinator must be inspectable, not a black box.
        events = ok(PORT_A, token_a, "thread.get", {"threadId": parent})["events"]
        kinds = [e["event"]["kind"] for e in events]
        check("user_message" in kinds, "the brief is in the coordinator's transcript")
        brief = next(e["event"] for e in events if e["event"]["kind"] == "user_message")
        check(brief.get("injected") is True,
              "and is flagged as machine-issued, not something the user typed")
        check(kinds.count("subagent_started") == 2,
              "a crew row per worker is in the parent's feed")

        # The coordinator runs NO turn of its own. If it did, there would be a
        # turn_started here and the whole shape decision would be wrong.
        check("turn_started" not in kinds,
              "the coordinator started no turn of its own")

        # ---------------------------------------------- the unreachable case ---
        print("\n== an unreachable target is reported, not skipped ==")
        ghost = ok(PORT_A, token_a, "schedule.create", {
            "projectId": project_a, "agent": "claude", "settings": settings(),
            "name": "half broken", "prompt": "build it",
            "cadence": {"type": "daily", "time": "05:00"},
            "dispatch": {"machines": [machine_a, "no-such-machine"], "syncRef": False},
        })
        fired2 = ok(PORT_A, token_a, "schedule.run", {"scheduleId": ghost["id"]},
                    timeout=120)
        parent2 = fired2["threadId"]
        events2 = ok(PORT_A, token_a, "thread.get", {"threadId": parent2})["events"]
        statuses = [e["event"]["text"] for e in events2
                    if e["event"]["kind"] == "status"]
        check(any("no-such-machine" in s for s in statuses),
              "the refused target is named in the coordinator's transcript")
        check(any(s.startswith("1 of 2") for s in statuses),
              "and the count is honest about how many actually started")
        check(len(ok(PORT_A, token_a, "dispatch.list",
                     {"threadId": parent2})["dispatches"]) == 1,
              "the reachable target still ran")

        print("\n== every target unreachable fails the run, loudly ==")
        dead = ok(PORT_A, token_a, "schedule.create", {
            "projectId": project_a, "agent": "claude", "settings": settings(),
            "name": "all broken", "prompt": "build it",
            "cadence": {"type": "daily", "time": "06:00"},
            "dispatch": {"machines": ["nobody-here"], "syncRef": False},
        })
        bad = rpc(PORT_A, token_a, "schedule.run", {"scheduleId": dead["id"]},
                  timeout=120)
        check(not bad.get("ok"), "the run reports failure rather than a thread id")
        after = next(s for s in ok(PORT_A, token_a, "schedule.list", {})["schedules"]
                     if s["id"] == dead["id"])
        check(bool(after.get("lastError")), "and the schedule records why")

        # ------------------------------------------------------- settling ---
        print("\n== the coordinator settles when its crew is done ==")
        # No agent credentials in the sandboxed HOME, so both workers fail —
        # which is exactly the path that has to still deliver a report.
        deadline = time.time() + 90
        settled = None
        while time.time() < deadline:
            live = [r for r in ok(PORT_A, token_a, "dispatch.list",
                                  {"threadId": parent})["dispatches"]
                    if r["status"] in ("queued", "running")]
            thread = ok(PORT_A, token_a, "thread.get", {"threadId": parent})["thread"]
            if not live:
                settled = thread["status"]
                break
            time.sleep(2)
        check(settled is not None, "every worker reached a terminal status")
        check(settled == "idle",
              f"the coordinator went idle once the crew finished (was {settled})")

        finals = ok(PORT_A, token_a, "dispatch.list", {"threadId": parent})["dispatches"]
        check(all(r.get("result") for r in finals),
              "each worker left a result behind, success or not")
        done_kinds = [e["event"]["kind"] for e in
                      ok(PORT_A, token_a, "thread.get", {"threadId": parent})["events"]]
        check(done_kinds.count("subagent_completed") == 2,
              "and the parent's feed shows both completions")

    finally:
        for _tag, (proc, _, _) in procs.items():
            proc.terminate()
        time.sleep(1)
        for _tag, (proc, _, _) in procs.items():
            if proc.poll() is None:
                proc.kill()

    print()
    if failures:
        print(f"{len(failures)} FAILED:")
        for f in failures:
            print(f"  - {f}")
        sys.exit(1)
    print("all checks passed")


main()
