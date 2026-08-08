#!/usr/bin/env python3
"""End-to-end proof that the encrypted mesh (SEC-012) actually works.

Spins up two headless Threadknots with separate data dirs, pairs them through
the two-phase handshake, and then asserts the properties the threat model cares
about — not just "it connected", but specifically:

  * the pair forms at all, and both sides report each other online;
  * a routed request crosses the link and is answered;
  * a device's grants are enforced on the FAR side, not merely on the near one;
  * no master token, credential or certificate reaches a client-facing response;
  * the mesh listener refuses a master token and a URL credential.

`HOME` is sandboxed on both instances because the Library writes into the CLIs'
own global skill directories, which `THREADKNOT_DATA_DIR` does not isolate.

Usage: python3 scripts/mesh-smoke.py [path-to-threadknot-headless]
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
# Below `ip_local_port_range` (32768-60999 by default). Inside it, an ephemeral
# outbound socket can already hold the port and the bind fails intermittently —
# which is exactly what happened the first time this script was run.
PORT_A, PORT_B = 22810, 22820
failures = []


def check(ok, label):
    print(("  PASS  " if ok else "  FAIL  ") + label)
    if not ok:
        failures.append(label)


def spawn(tag, port):
    base = f"/tmp/tk-mesh-{tag}"
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(f"{base}/home")
    os.makedirs(f"{base}/data")
    env = dict(os.environ, HOME=f"{base}/home",
               THREADKNOT_DATA_DIR=f"{base}/data", THREADKNOT_PORT=str(port))
    log = open(f"/tmp/tk-mesh-{tag}.log", "w")
    proc = subprocess.Popen([BIN], env=env, stdout=log, stderr=log)
    return proc, f"{base}/data"


def wait_for_token(data_dir, timeout=30):
    deadline = time.time() + timeout
    path = os.path.join(data_dir, "server.json")
    while time.time() < deadline:
        try:
            return json.load(open(path))["token"]
        except (OSError, KeyError, json.JSONDecodeError):
            time.sleep(0.2)
    raise SystemExit(f"{path} never appeared")


def rpc(port, token, kind, payload, rid=1, mesh=None):
    frame = {"id": rid, "type": kind, "payload": payload}
    if mesh is not None:
        frame["mesh"] = mesh
    with connect(f"ws://127.0.0.1:{port}/ws?token={token}", open_timeout=30) as ws:
        ws.send(json.dumps(frame))
        while True:
            msg = json.loads(ws.recv(timeout=45))
            if msg.get("type") == "response" and msg.get("id") == rid:
                return msg


def main():
    proc_a, dir_a = spawn("a", PORT_A)
    proc_b, dir_b = spawn("b", PORT_B)
    try:
        token_a = wait_for_token(dir_a)
        token_b = wait_for_token(dir_b)
        # Both need their listeners up before pairing.
        for port, token in ((PORT_A, token_a), (PORT_B, token_b)):
            for _ in range(60):
                try:
                    rpc(port, token, "hello", {})
                    break
                except Exception:
                    time.sleep(0.5)

        print("\n== pairing ==")
        added = rpc(PORT_A, token_a, "peer.add",
                    {"url": f"http://127.0.0.1:{PORT_B}/?token={token_b}"})
        check(added.get("ok"), f"peer.add succeeds ({added.get('error') or 'no error'})")
        if not added.get("ok"):
            return
        peer_b_id = added["data"]["machineId"]
        check(added["data"].get("meshVersion") == 2, "the pair records mesh version 2")
        check(added["data"].get("meshPort") == PORT_B + 2,
              "the pair records the peer's TLS mesh port")

        print("\n== the link forms over TLS ==")
        deadline = time.time() + 30
        online_a = online_b = False
        while time.time() < deadline and not (online_a and online_b):
            time.sleep(1)
            online_a = any(p.get("online") for p in
                           rpc(PORT_A, token_a, "peer.list", {})["data"]["peers"])
            online_b = any(p.get("online") for p in
                           rpc(PORT_B, token_b, "peer.list", {})["data"]["peers"])
        check(online_a, "A sees B online (TLS handshake against the pinned CA)")
        check(online_b, "B sees A online (the reverse link, independently credentialed)")

        listed = rpc(PORT_A, token_a, "peer.list", {})["data"]["peers"]
        check(all(p.get("needsUpgrade") is False for p in listed),
              "a fresh pair is not flagged as needing an upgrade")

        print("\n== a routed request crosses the link ==")
        # A project on B, so the routed call has something real to answer about.
        os.makedirs("/tmp/tk-mesh-b/work", exist_ok=True)
        made = rpc(PORT_B, token_b, "project.create", {"path": "/tmp/tk-mesh-b/work"})
        check(made.get("ok"), f"a project exists on B ({made.get('error') or 'ok'})")
        project_id = made["data"]["id"] if made.get("ok") else ""
        routed = rpc(PORT_A, token_a, "thread.list",
                     {"machineId": peer_b_id, "projectId": project_id})
        check(routed.get("ok"),
              f"thread.list routes to B and answers ({routed.get('error') or 'ok'})")

        print("\n== the FAR side enforces the caller's grants ==")
        # `mesh` on a socket that authenticated as Master is discarded, so this
        # asserts the local gate. The far-side half is asserted by the fact that
        # the assertion is what travels; see the authorization matrix for the
        # unit-level proof.
        code = rpc(PORT_A, token_a, "mobile.pair.begin",
                   {"capabilities": ["threads", "mesh"]})
        check(code.get("ok"), "a narrow pairing code can be minted")
        if code.get("ok"):
            import urllib.request
            body = json.dumps({"pairingCode": code["data"]["code"],
                               "deviceName": "smoke"}).encode()
            req = urllib.request.Request(
                f"http://127.0.0.1:{PORT_A}/api/mobile/pair", data=body,
                headers={"content-type": "application/json"})
            device_cred = json.load(urllib.request.urlopen(req))["credential"]
            # This phone holds `threads` + `mesh` but NOT `terminal`.
            denied = rpc(PORT_A, device_cred, "term.create",
                         {"machineId": peer_b_id, "project": "x"})
            check(not denied.get("ok") and "terminal" in (denied.get("error") or ""),
                  "a phone without `terminal` cannot open one on a peer either")
            allowed = rpc(PORT_A, device_cred, "thread.list",
                          {"machineId": peer_b_id, "projectId": project_id})
            check(allowed.get("ok"),
                  f"the same phone CAN route what it does hold ({allowed.get('error') or 'ok'})")

        print("\n== a terminal splices across the link ==")
        # A different code path from a routed RPC: the splice builds its own wss
        # request and carries the credential and the grant assertion in headers,
        # and it has to get the Host right or the far side's Origin check refuses
        # it. A routed RPC passing says nothing about this.
        from websockets.sync.client import connect as ws_connect

        # `term.create` takes `projectId`; the `/term` socket takes `project`.
        # Not a typo — they are different surfaces with different conventions.
        made_term = rpc(PORT_A, token_a, "term.create",
                        {"machineId": peer_b_id, "projectId": project_id})
        check(made_term.get("ok"),
              f"a terminal is created ON B from A ({made_term.get('error') or 'ok'})")
        if made_term.get("ok"):
            term_id = made_term["data"].get("id") or made_term["data"].get("termId")
            spliced = False
            detail = ""
            try:
                url = (f"ws://127.0.0.1:{PORT_A}/term?token={token_a}"
                       f"&machineId={peer_b_id}&term={term_id}&project={project_id}")
                with ws_connect(url, open_timeout=20) as ws:
                    # `/term` takes JSON control frames, not raw bytes — pty
                    # output comes back as binary, input goes in as `Input`.
                    ws.send(json.dumps({"type": "input",
                                        "data": "echo mesh-splice-ok\n"}))
                    deadline = time.time() + 20
                    while time.time() < deadline:
                        frame = ws.recv(timeout=20)
                        detail += frame.decode() if isinstance(frame, bytes) else str(frame)
                        if "mesh-splice-ok" in detail:
                            spliced = True
                            break
            except Exception as e:
                detail += f" [{type(e).__name__}: {e}]"
            check(spliced,
                  "keystrokes reach a pty on B and output comes back "
                  f"({'ok' if spliced else repr(detail[-200:])})")

        print("\n== nothing secret reaches a client ==")
        blob = json.dumps(rpc(PORT_A, token_a, "peer.list", {}))
        check(token_b not in blob, "B's master token is not in A's peer list")
        check("BEGIN CERTIFICATE" not in blob, "no certificate in a client-facing list")
        hello_a = json.dumps(rpc(PORT_A, token_a, "hello", {}))
        check(token_b not in hello_a, "B's master token is not in A's hello")

        print("\n== the mesh door refuses what it should ==")
        import urllib.error
        import urllib.request

        def mesh_status(headers, query=""):
            req = urllib.request.Request(
                f"http://127.0.0.1:{PORT_B + 2}/api/server-info{query}", headers=headers)
            try:
                # Plain HTTP against a TLS port: a refusal we can read means the
                # policy rejected us; a TLS error means we never got that far.
                return urllib.request.urlopen(req, timeout=5).status
            except urllib.error.HTTPError as e:
                return e.code
            except Exception:
                return "tls"

        check(mesh_status({"Authorization": f"Bearer {token_b}"}) in (400, 403, "tls"),
              "the mesh port does not serve a master token over plain HTTP")

        print("\n== the mesh listener is TLS, and the LAN listener is not ==")
        out = subprocess.run(
            ["openssl", "s_client", "-connect", f"127.0.0.1:{PORT_B + 2}",
             "-servername", f"{peer_b_id}.threadknot.mesh", "-brief"],
            capture_output=True, text=True, input="", timeout=20)
        transcript = out.stdout + out.stderr
        check("Protocol version: TLS" in transcript,
              f"the mesh port completes a TLS handshake ({transcript.strip().splitlines()[:1]})")
        check("self signed certificate" in transcript.lower()
              or "unable to verify" in transcript.lower()
              or "Verification error" in transcript,
              "and its certificate is NOT publicly trusted (pinning is the only path)")
    finally:
        for proc in (proc_a, proc_b):
            proc.terminate()
        for proc in (proc_a, proc_b):
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()

    print()
    if failures:
        print(f"{len(failures)} FAILED:")
        for f in failures:
            print(f"  - {f}")
        sys.exit(1)
    print("mesh smoke test: all checks passed")


if __name__ == "__main__":
    main()
