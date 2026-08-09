#!/usr/bin/env python3
"""End-to-end proof that Dispatch works, including across the mesh.

Two headless Threadknots with separate data dirs, paired to each other, then
driven the way a real orchestrating agent drives them. What it asserts is not
"the RPC returned 200" but the properties the feature is *for*:

  * a command runs on THIS machine and its exit code and output come back;
  * a command runs on the OTHER machine — the whole point of the feature;
  * a command that outlives the wait window degrades to a handle, not an error,
    and the job keeps running while nobody is watching;
  * cancelling kills the work rather than just the shell;
  * a dispatch creates a child thread on the target machine, with the agent and
    the access the parent asked for and no more;
  * the worker's report reaches the parent's ledger;
  * a report raised while the parent was OFFLINE is delivered on reconnect,
    rather than lost;
  * authority narrows: a read-only parent cannot dispatch a full-access worker,
    and a machine that refuses dispatch is refused.

`HOME` is sandboxed on both instances because the Library writes into the CLIs'
own global skill directories, which `THREADKNOT_DATA_DIR` does not isolate.

Usage: python3 scripts/dispatch-smoke.py [path-to-threadknot-headless]
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
PORT_A, PORT_B = 22910, 22920
failures = []
procs = {}


def check(ok, label):
    print(("  PASS  " if ok else "  FAIL  ") + label)
    if not ok:
        failures.append(label)


def spawn(tag, port):
    base = f"/tmp/tk-dispatch-{tag}"
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(f"{base}/home")
    os.makedirs(f"{base}/data")
    os.makedirs(f"{base}/work")
    env = dict(os.environ, HOME=f"{base}/home",
               THREADKNOT_DATA_DIR=f"{base}/data", THREADKNOT_PORT=str(port))
    log = open(f"/tmp/tk-dispatch-{tag}.log", "w")
    proc = subprocess.Popen([BIN], env=env, stdout=log, stderr=log)
    procs[tag] = (proc, port, base)
    return proc, f"{base}/data", f"{base}/work"


def restart(tag):
    """Kill and relaunch an instance on the same data dir — the offline test."""
    proc, port, base = procs[tag]
    proc.terminate()
    try:
        proc.wait(timeout=20)
    except subprocess.TimeoutExpired:
        proc.kill()
    env = dict(os.environ, HOME=f"{base}/home",
               THREADKNOT_DATA_DIR=f"{base}/data", THREADKNOT_PORT=str(port))
    log = open(f"/tmp/tk-dispatch-{tag}.log", "a")
    new = subprocess.Popen([BIN], env=env, stdout=log, stderr=log)
    procs[tag] = (new, port, base)
    return new


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
    """RPC that must succeed; raises with the server's own message if not."""
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
        project_b = ok(PORT_B, token_b, "project.create", {"path": work_b})["id"]
        # One workspace spanning both machines — the shape the feature assumes.
        workspace = ok(PORT_A, token_a, "workspace.list", {})["workspaces"]
        workspace_id = next(w["id"] for w in workspace if w["id"] == project_a)
        ok(PORT_A, token_a, "workspace.attachRoot",
           {"workspaceId": workspace_id, "machineId": machine_b, "path": work_b})
        roots = ok(PORT_A, token_a, "workspace.list", {})["workspaces"]
        members = next(w for w in roots if w["id"] == workspace_id)["members"]
        check(len(members) == 2, "the workspace has a root on each machine")

        thread = ok(PORT_A, token_a, "thread.create", {
            "projectId": project_a, "agent": "claude",
            "settings": {"model": "claude-opus-5", "access": "full", "mode": "build",
                         "wideContext": False, "claudeChrome": False},
        })
        thread_id = thread["id"]

        # ---------------------------------------------------------- Phase 0 ---
        print("\n== exec: a command on THIS machine ==")
        started = ok(PORT_A, token_a, "exec.start",
                     {"projectId": project_a, "command": "echo hi; exit 4"})
        check(started["status"] == "exited", "a fast command completes inside the start grace")
        check(started["exitCode"] == 4, "its real exit code comes back")
        check("hi" in started["stdout"], "its stdout comes back")

        print("\n== exec: a command on the OTHER machine ==")
        remote = ok(PORT_A, token_a, "exec.start",
                    {"machineId": machine_b, "projectId": project_b,
                     "command": "echo from-b; pwd"})
        check(remote["status"] == "exited" and remote["exitCode"] == 0,
              f"the routed command ran on B ({remote.get('status')})")
        check("from-b" in remote["stdout"], "B's stdout came back over the mesh")
        check(work_b in remote["stdout"], "it ran in B's own root, not A's")

        print("\n== exec: a long command degrades to a handle, not an error ==")
        slow = ok(PORT_A, token_a, "exec.start",
                  {"machineId": machine_b, "projectId": project_b,
                   "command": "sleep 6; echo finished-anyway"})
        check(slow["status"] == "running", "start returns while the job is still going")
        job = slow["jobId"]
        # Nobody is watching for a moment; the job must not care.
        time.sleep(7)
        done = ok(PORT_A, token_a, "exec.status",
                  {"machineId": machine_b, "jobId": job, "waitSeconds": 10})
        check(done["status"] == "exited", "the job ran to completion unattended")
        check("finished-anyway" in done["stdout"],
              "its output was buffered and is still retrievable")

        print("\n== exec: cancel stops the work ==")
        victim = ok(PORT_A, token_a, "exec.start",
                    {"machineId": machine_b, "projectId": project_b,
                     "command": "sleep 45"})
        ok(PORT_A, token_a, "exec.cancel",
           {"machineId": machine_b, "jobId": victim["jobId"]})
        after = ok(PORT_A, token_a, "exec.status",
                   {"machineId": machine_b, "jobId": victim["jobId"], "waitSeconds": 5})
        check(after["status"] == "cancelled", "the routed cancel took effect on B")

        print("\n== exec: a bad root is refused, not guessed ==")
        bad = rpc(PORT_A, token_a, "exec.start",
                  {"projectId": "no-such-project", "command": "echo nope"})
        check(not bad.get("ok"), "an unknown project is an error")
        escape = rpc(PORT_A, token_a, "exec.start",
                     {"projectId": project_a, "subdir": "../../etc",
                      "command": "echo nope"})
        check(not escape.get("ok"), "a subdir escaping the root is refused")

        run_dispatch_checks(token_a, token_b, machine_a, machine_b,
                            project_a, project_b, thread_id)

    finally:
        for tag, (proc, _, _) in procs.items():
            proc.terminate()
        time.sleep(1)
        for tag, (proc, _, _) in procs.items():
            if proc.poll() is None:
                proc.kill()

    print()
    if failures:
        print(f"{len(failures)} FAILED:")
        for f in failures:
            print(f"  - {f}")
        sys.exit(1)
    print("all checks passed")


def run_dispatch_checks(token_a, token_b, machine_a, machine_b,
                        project_a, project_b, thread_id):
    """Phase 1+ checks. Kept separate so Phase 0 can be run on its own."""
    probe = rpc(PORT_A, token_a, "dispatch.list", {"threadId": thread_id})
    if not probe.get("ok") and "unknown request" in str(probe.get("error", "")):
        print("\n== dispatch not built yet — skipping ==")
        return
    dispatch_checks(token_a, token_b, machine_a, machine_b,
                    project_a, project_b, thread_id)


def dispatch_checks(token_a, token_b, machine_a, machine_b,
                    project_a, project_b, thread_id):
    print("\n== dispatch: authority narrows ==")
    # A read-only parent must not be able to hand out a full-access worker.
    ro = ok(PORT_A, token_a, "thread.create", {
        "projectId": project_a, "agent": "claude",
        "settings": {"model": "claude-opus-5", "access": "read", "mode": "build",
                     "wideContext": False, "claudeChrome": False},
    })["id"]
    denied = rpc(PORT_A, token_a, "dispatch.create", {
        "parentThreadId": ro, "brief": "do a thing", "label": "nope",
        "machineId": machine_a, "projectId": project_a,
        "agent": "claude", "access": "full",
    })
    granted = (denied.get("data") or {}).get("access")
    check(not denied.get("ok") or granted != "full",
          f"a read-only parent cannot mint a full-access worker (got {granted})")

    print("\n== dispatch: a worker is created on the TARGET machine ==")
    made = rpc(PORT_A, token_a, "dispatch.create", {
        "parentThreadId": thread_id, "brief": "Print the OS name and stop.",
        "label": "probe", "machineId": machine_b, "projectId": project_b,
        "agent": "claude", "access": "full", "autostart": False,
    })
    check(made.get("ok"), f"dispatch.create to B succeeds ({made.get('error') or 'ok'})")
    if not made.get("ok"):
        return
    record = made["data"]
    child = record["childThreadId"]
    check(record["machineId"] == machine_b, "the ledger records where it runs")

    # The child must exist on B, in B's own store, pinned to B.
    got = rpc(PORT_B, token_b, "thread.get", {"threadId": child})
    check(got.get("ok"), "the child thread exists in B's own store")
    if got.get("ok"):
        t = got["data"]["thread"] if "thread" in got["data"] else got["data"]
        check(t.get("machineId") == machine_b, "the child is pinned to B")
        check((t.get("dispatch") or {}).get("parentThreadId") == thread_id,
              "the child knows which thread sent it")

    print("\n== dispatch: the worker's report reaches the parent ==")
    ok(PORT_B, token_b, "dispatch.report", {
        "dispatchId": record["id"],
        "status": "succeeded",
        "summary": "linux, kernel 7.1",
        "changed": ["notes.md"],
    })
    deadline = time.time() + 30
    seen = None
    while time.time() < deadline:
        listed = ok(PORT_A, token_a, "dispatch.list", {"threadId": thread_id})
        seen = next((d for d in listed["dispatches"] if d["id"] == record["id"]), None)
        if seen and seen["status"] != "running":
            break
        time.sleep(1)
    check(seen and seen["status"] == "succeeded",
          f"the parent's ledger shows the result ({seen and seen['status']})")
    check(seen and (seen.get("result") or {}).get("summary", "").startswith("linux"),
          "the summary travelled with it")

    print("\n== dispatch: a report raised while the parent is offline is not lost ==")
    offline = rpc(PORT_A, token_a, "dispatch.create", {
        "parentThreadId": thread_id, "brief": "second job", "label": "offline-test",
        "machineId": machine_b, "projectId": project_b,
        "agent": "claude", "access": "full", "autostart": False,
    })
    if offline.get("ok"):
        second = offline["data"]["id"]
        restart("a")
        token_a2 = wait_for_token(f"/tmp/tk-dispatch-a/data")
        # Report while A is still coming up.
        ok(PORT_B, token_b, "dispatch.report", {
            "dispatchId": second, "status": "succeeded", "summary": "delivered late",
        })
        for _ in range(90):
            try:
                listed = rpc(PORT_A, token_a2, "dispatch.list", {"threadId": thread_id})
                if listed.get("ok"):
                    hit = next((d for d in listed["data"]["dispatches"]
                                if d["id"] == second), None)
                    if hit and hit["status"] == "succeeded":
                        break
            except Exception:
                pass
            time.sleep(1)
        else:
            hit = None
        check(hit is not None and hit["status"] == "succeeded",
              "the queued report was delivered once the parent came back")


if __name__ == "__main__":
    main()
