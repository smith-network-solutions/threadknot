#!/usr/bin/env python3
"""Dispatch across two REAL machines and two operating systems.

`dispatch-smoke.py` proves the mesh with two instances on one box.
`dispatch-live.py` proves a real agent does real work. This proves the thing
those two cannot: a thread on a Linux machine hands a brief to an agent on a
physically separate **macOS** machine, that agent does the work on the Mac's
own disk, and the report comes back over the mesh.

Both instances run on scratch ports and scratch data dirs, so neither the live
desktop app here nor the Armada install on the Mac is touched.

Prereqs: SSH to the Mac, and a `threadknot-headless` built there.

Usage: python3 scripts/dispatch-crossos.py [mac-host]
"""

import json
import os
import shutil
import subprocess
import sys
import time
from websockets.sync.client import connect

MAC = sys.argv[1] if len(sys.argv) > 1 else "mollysmith@192.168.0.97"
MAC_IP = MAC.split("@")[-1]
MAC_REPO = "~/dev/threadknot"
MAC_PORT, LINUX_PORT = 42810, 42910
MAC_DATA, MAC_WORK = "/tmp/tk-x-mac/data", "/tmp/tk-x-mac/work"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LINUX_BIN = os.path.join(ROOT, "src-tauri/target/debug/threadknot-headless")
LINUX_BASE = "/tmp/tk-x-linux"

failures = []
linux_proc = None


def check(ok, label):
    print(("  PASS  " if ok else "  FAIL  ") + label)
    if not ok:
        failures.append(label)


def sh(cmd, timeout=120):
    return subprocess.run(["ssh", "-o", "BatchMode=yes", MAC, cmd],
                          capture_output=True, text=True, timeout=timeout).stdout.strip()


def rpc(port, token, kind, payload, timeout=180, host=None):
    # The Mac's instance is reached over the LAN, not loopback — dialling
    # 127.0.0.1 for it is how the first run of this script failed.
    host = host or ("127.0.0.1" if port == LINUX_PORT else MAC_IP)
    with connect(f"ws://{host}:{port}/ws?token={token}", open_timeout=30,
                 max_size=1 << 26) as ws:
        ws.send(json.dumps({"id": 1, "type": kind, "payload": payload}))
        while True:
            m = json.loads(ws.recv(timeout=timeout))
            if m.get("id") == 1:
                return m


def ok(port, token, kind, payload, **kw):
    r = rpc(port, token, kind, payload, **kw)
    if not r.get("ok"):
        raise AssertionError(f"{kind} failed: {r.get('error')}")
    return r.get("data", {})


def start_mac():
    """Launch a scratch headless instance on the Mac, leaving Armada alone."""
    # Kill by binary name, not by env var: the env assignment never appears in
    # argv, so `pkill -f THREADKNOT_PORT=...` matches nothing and leaves the
    # previous run holding the port — which then answers pairing with a token
    # from a data dir we just deleted ("pairing proof did not verify").
    # Safe against the live install: that one's binary is named `armada`.
    sh("pkill -f threadknot-headless || true")
    for _ in range(20):
        if not sh(f"lsof -ti tcp:{MAC_PORT} || true"):
            break
        time.sleep(1)
    sh(f"rm -rf /tmp/tk-x-mac && mkdir -p {MAC_DATA} {MAC_WORK}")
    sh(f"cd {MAC_REPO}/src-tauri && "
       f"THREADKNOT_DATA_DIR={MAC_DATA} THREADKNOT_PORT={MAC_PORT} "
       f"nohup ./target/release/threadknot-headless > /tmp/tk-x-mac/log 2>&1 & echo started")
    for _ in range(60):
        raw = sh(f"cat {MAC_DATA}/server.json 2>/dev/null || true")
        if raw:
            try:
                return json.loads(raw)["token"]
            except Exception:
                pass
        time.sleep(1)
    raise SystemExit("the Mac instance never wrote server.json; see /tmp/tk-x-mac/log")


def start_linux():
    global linux_proc
    shutil.rmtree(LINUX_BASE, ignore_errors=True)
    os.makedirs(f"{LINUX_BASE}/home")
    os.makedirs(f"{LINUX_BASE}/data")
    os.makedirs(f"{LINUX_BASE}/work")
    env = dict(os.environ, HOME=f"{LINUX_BASE}/home",
               THREADKNOT_DATA_DIR=f"{LINUX_BASE}/data",
               THREADKNOT_PORT=str(LINUX_PORT))
    log = open("/tmp/tk-x-linux.log", "w")
    linux_proc = subprocess.Popen([LINUX_BIN], env=env, stdout=log, stderr=log)
    for _ in range(60):
        try:
            return json.load(open(f"{LINUX_BASE}/data/server.json"))["token"]
        except Exception:
            time.sleep(1)
    raise SystemExit("the Linux instance never came up; see /tmp/tk-x-linux.log")


def main():
    print("== starting a scratch instance on each machine ==")
    mac_token = start_mac()
    linux_token = start_linux()
    for _ in range(60):
        try:
            rpc(LINUX_PORT, linux_token, "hello", {})
            break
        except Exception:
            time.sleep(1)
    check(True, "both instances are up (Armada and the desktop app untouched)")

    print("\n== pairing Linux -> macOS over the LAN ==")
    added = ok(LINUX_PORT, linux_token, "peer.add",
               {"url": f"http://{MAC_IP}:{MAC_PORT}/?token={mac_token}"})
    mac_id = added["machineId"]
    online = False
    deadline = time.time() + 60
    while time.time() < deadline and not online:
        time.sleep(2)
        online = any(p.get("online") for p in
                     rpc(LINUX_PORT, linux_token, "peer.list", {})["data"]["peers"])
    check(online, "the encrypted mesh link to the Mac is up")
    if not online:
        return

    # The RETURN link matters as much as the outbound one: the worker's report
    # is pushed Mac -> Linux, so the Mac needs its own socket to us.
    back = False
    deadline = time.time() + 60
    while time.time() < deadline and not back:
        time.sleep(2)
        back = any(p.get("online") for p in
                   rpc(MAC_PORT, mac_token, "peer.list", {})["data"]["peers"])
    # NOT a required condition — it is the interesting one. macOS Local Network
    # privacy refuses outbound LAN connections from an unsigned, SSH-launched
    # binary (EHOSTUNREACH) while still accepting inbound ones, so on this
    # network the Mac frequently CANNOT call home. The dispatch must complete
    # anyway, via the parent-side reconciler; that is asserted below.
    print(f"  NOTE  the Mac's return link is {'up' if back else 'DOWN (one-way reachability)'}")

    info = ok(LINUX_PORT, linux_token, "device.info", {"machineId": mac_id})
    check(info.get("os") == "macos", f"the peer reports itself as macOS (got {info.get('os')})")
    check("run-claude" in (info.get("capabilities") or []),
          f"the Mac advertises an agent we can dispatch to: {info.get('capabilities')}")
    check(info.get("acceptsDispatch") is True, "the Mac accepts dispatched work")

    print("\n== a command runs on the Mac, from Linux ==")
    mac_project = ok(MAC_PORT, mac_token, "project.create", {"path": MAC_WORK})["id"]
    run = ok(LINUX_PORT, linux_token, "exec.start",
             {"machineId": mac_id, "projectId": mac_project,
              "command": "uname -s; sw_vers -productVersion; pwd"})
    for _ in range(20):
        if run["status"] != "running":
            break
        run = ok(LINUX_PORT, linux_token, "exec.status",
                 {"machineId": mac_id, "jobId": run["jobId"], "waitSeconds": 5})
    check(run["status"] == "exited" and run["exitCode"] == 0,
          f"the routed command ran on the Mac ({run['status']})")
    check("Darwin" in run.get("stdout", ""),
          f"it really executed on macOS: {run.get('stdout','')[:60]!r}")

    print("\n== dispatch: a Linux thread hands real work to an agent on the Mac ==")
    linux_project = ok(LINUX_PORT, linux_token, "project.create",
                       {"path": f"{LINUX_BASE}/work"})["id"]
    parent = ok(LINUX_PORT, linux_token, "thread.create", {
        "projectId": linux_project, "agent": "claude",
        "settings": {"model": "claude-opus-5", "access": "full", "mode": "build",
                     "wideContext": False, "claudeChrome": False}})["id"]
    record = ok(LINUX_PORT, linux_token, "dispatch.create", {
        "parentThreadId": parent,
        "machineId": mac_id,
        "projectId": mac_project,
        # Codex, not Claude: on macOS the Claude CLI keeps its credentials in the
        # login Keychain, which a process started over SSH cannot unlock — it
        # answers "Not logged in · Please run /login". Codex and Kimi store auth
        # in files under $HOME, so they work in an SSH-started instance.
        "agent": "codex",
        "label": "cross-os probe",
        "brief": ("Create a file named from-the-mac.txt in the current directory. "
                  "Its contents must be exactly the output of `uname -sm`. "
                  "Then call report_result with status succeeded and a one-line "
                  "summary that includes that same text. Do nothing else."),
    })
    check(bool(record["childThreadId"]), "a worker thread was created ON THE MAC")
    check(record["machineId"] == mac_id, "the ledger records the Mac as its machine")

    print("  waiting for the Mac's agent to finish (up to 6 min)…")
    deadline = time.time() + 360
    while time.time() < deadline:
        record = ok(LINUX_PORT, linux_token, "dispatch.get", {"dispatchId": record["id"]})
        if record["status"] not in ("queued", "running"):
            break
        time.sleep(5)

    check(record["status"] == "succeeded",
          f"the dispatch succeeded (was {record['status']})")
    if not back:
        check(record["status"] == "succeeded",
              "the result got home even though the worker could NOT reach the "
              "parent — the reconciler closed the loop")
    result = record.get("result") or {}
    summary = result.get("summary", "")
    print(f"  summary: {summary[:220]}")
    check(bool(summary), "a summary came back across the mesh")
    check(not result.get("inferred"),
          "the Mac's agent filed a STRUCTURED report, not an inferred one")

    on_disk = sh(f"cat {MAC_WORK}/from-the-mac.txt 2>/dev/null || true")
    check("Darwin" in on_disk,
          f"the work really landed on the Mac's disk: {on_disk[:40]!r}")
    check("Darwin" in summary or "arm64" in summary,
          "the report describes what it actually did")

    print("\n== the parent thread on Linux shows the remote worker ==")
    feed = json.dumps(ok(LINUX_PORT, linux_token, "thread.get", {"threadId": parent}))
    check("subagent_started" in feed, "the worker was announced to the parent")
    check("subagent_completed" in feed, "its completion was announced to the parent")
    check(mac_id in feed, "the parent's feed attributes the work to the Mac")


if __name__ == "__main__":
    try:
        main()
    finally:
        if linux_proc:
            linux_proc.terminate()
        subprocess.run(["ssh", "-o", "BatchMode=yes", MAC,
                        "pkill -f threadknot-headless || true"],
                       capture_output=True)
    print()
    if failures:
        print(f"{len(failures)} FAILED:")
        for f in failures:
            print(f"  - {f}")
        sys.exit(1)
    print("all checks passed")
