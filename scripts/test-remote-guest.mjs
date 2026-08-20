#!/usr/bin/env node
// The guest link against a REAL remote box, over whatever address it answers on
// — a bare public IP, or the relay origin when no inbound port is open.
//
// Shape of the test:
//   - the box's master token is used only through an SSH tunnel to its
//     loopback listener, so it never crosses the public internet in cleartext;
//   - the pairing code is short-lived and single-use, and IS sent over the
//     public address, which is what it is designed for;
//   - everything after that is a local desktop instance talking to that address
//     exactly as a real desktop would.
//
//   node scripts/test-remote-guest.mjs <public-ip|origin> <tunnel-port> <master-token>

import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connect } from "node:net";

const [TARGET, TUNNEL, MASTER, BOX_PATH = "/srv/clients"] = process.argv.slice(2);
if (!TARGET || !TUNNEL || !MASTER) {
  console.error("usage: test-remote-guest.mjs <ip|origin> <tunnelPort> <masterToken> [boxPath]");
  process.exit(2);
}
const BIN = join(import.meta.dirname, "..", "src-tauri", "target", "release", "threadknot-headless");
const DESKTOP_PORT = 42895;
// A bare IP means the box's own port; a full origin means the relay, which is
// the path that has to work when no inbound port is open at all.
const PUBLIC_ORIGIN = TARGET.startsWith("http") ? TARGET.replace(/\/$/, "") : `http://${TARGET}:42800`;

let failures = 0;
const check = (label, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  if (!ok) failures++;
  console.log(
    `${ok ? "  ok  " : "  FAIL"}  ${label}` +
      (ok ? "" : `\n          expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`),
  );
};
const checkTrue = (label, a) => check(label, !!a, true);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

class Client {
  constructor(url) {
    this.ws = new WebSocket(url);
    this.pending = new Map();
    this.id = 0;
    this.ready = new Promise((res, rej) => {
      this.ws.addEventListener("open", () => res());
      this.ws.addEventListener("error", (e) => rej(new Error(String(e.message ?? e))));
    });
    this.ws.addEventListener("message", (ev) => {
      const f = JSON.parse(ev.data);
      if (f.type !== "response") return;
      const slot = this.pending.get(f.id);
      if (!slot) return;
      this.pending.delete(f.id);
      f.ok ? slot.resolve(f.data) : slot.reject(new Error(f.error ?? "failed"));
    });
  }
  request(kind, payload = {}) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, type: kind, payload }));
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      setTimeout(() => {
        if (this.pending.delete(id)) reject(new Error(`${kind} timed out`));
      }, 30000);
    });
  }
  close() {
    this.ws.close();
  }
}

async function listening(port) {
  for (let i = 0; i < 80; i++) {
    const up = await new Promise((res) => {
      const s = connect({ port, host: "127.0.0.1" });
      s.once("connect", () => (s.destroy(), res(true)));
      s.once("error", () => res(false));
    });
    if (up) return;
    await sleep(250);
  }
  throw new Error(`nothing listening on ${port}`);
}

// A local instance of the NEW build standing in for the desktop app, so the
// running desktop is left alone.
const dir = mkdtempSync(join(tmpdir(), "threadknot-remote-test-"));
const desktop = spawn(BIN, [], {
  env: { ...process.env, THREADKNOT_DATA_DIR: dir, THREADKNOT_PORT: String(DESKTOP_PORT) },
  stdio: ["ignore", "pipe", "pipe"],
});
const desktopToken = await new Promise((resolve, reject) => {
  let buf = "";
  const on = (c) => {
    buf += c.toString();
    const m = buf.match(/token=([A-Za-z0-9_-]+)/);
    if (m) resolve(m[1]);
  };
  desktop.stdout.on("data", on);
  desktop.stderr.on("data", on);
  setTimeout(() => reject(new Error(`no token:\n${buf}`)), 25000);
});

try {
  await listening(DESKTOP_PORT);
  await listening(Number(TUNNEL));

  // The box, over the SSH tunnel (master token stays off the public wire).
  const box = new Client(`ws://127.0.0.1:${TUNNEL}/ws?token=${MASTER}`);
  await box.ready;
  const boxHello = await box.request("hello");
  checkTrue("the box is running the new build", "person" in boxHello);
  console.log(`        box version ${boxHello.version}, machine ${boxHello.machineId?.slice(0, 8)}`);

  const peersOnBox = await box.request("peer.list");
  check("the box starts with no peer pairings", peersOnBox.peers.length, 0);

  // Real work on the box, so the guest has something to see.
  // A folder that really exists on the box. It used to be the container's
  // /data; going native moved it, and a workspace whose path is gone fails at
  // project.create rather than anywhere useful.
  const proj = await box.request("project.create", { path: BOX_PATH, name: "Client sites" });
  const projectId = proj.id ?? proj.project?.id;
  const boxThread = await box.request("thread.create", {
    projectId,
    agent: "claude",
    settings: { model: "sonnet", access: "read", mode: "build" },
  });
  await box.request("thread.rename", { threadId: boxThread.id, title: "Deploy notes" });

  const spencer = await box.request("person.create", { name: "Spencer" });
  checkTrue("a person exists on the box", !!spencer.id);

  // A one-time code, minted on the box and carried to the desktop by hand.
  const qr = await box.request("mobile.pair.begin", {
    capabilities: ["threads", "files", "git", "mesh"],
  });
  checkTrue("a pairing code was minted", !!qr.code);

  // The desktop: its own private work first, so "did anything leak" has a
  // named subject.
  const me = new Client(`ws://127.0.0.1:${DESKTOP_PORT}/ws?token=${desktopToken}`);
  await me.ready;
  const mine = await me.request("project.create", { path: dir, name: "My private work" });
  checkTrue("the desktop has a project of its own", !!(mine.id ?? mine.project?.id));

  // ---- the guest link, over the PUBLIC IP, with only the pairing code ----
  const added = await me.request("server.add", {
    origin: PUBLIC_ORIGIN,
    pairingCode: qr.code,
    deviceName: "Spencer's desktop",
  });
  checkTrue(`connected to the box over ${PUBLIC_ORIGIN}`, !!added.id);

  await box.request("device.setPerson", { deviceId: added.deviceId, personId: spencer.id });

  let server = null;
  for (let i = 0; i < 60; i++) {
    const { servers } = await me.request("server.list");
    server = servers.find((s) => s.id === added.id);
    if (server?.online && server.machineId) {
      // The sidebar's own refresh is what re-reads identity; do what it does.
      await me.request("server.catalog", { serverId: added.id }).catch(() => {});
      const again = await me.request("server.list");
      server = again.servers.find((s) => s.id === added.id);
      if (server?.personId) break;
    }
    await sleep(500);
  }
  checkTrue("the link is up", !!server?.online);
  check("we are Spencer over there", server?.personName, "Spencer");
  check(
    "and hold only the grants they issued",
    [...(server?.capabilities ?? [])].sort(),
    ["files", "git", "mesh", "threads"],
  );

  // ---- their work is visible and drivable from here ----
  const catalog = await me.request("server.catalog", { serverId: added.id });
  checkTrue("their workspaces are readable", catalog.workspaces.length > 0);
  const theirThreads = await me.request("thread.list", {
    projectId,
    machineId: server.machineId,
  });
  check("their chat routes over that address", theirThreads.threads[0]?.title, "Deploy notes");

  const started = await me.request("thread.create", {
    projectId,
    machineId: server.machineId,
    agent: "claude",
    settings: { model: "sonnet", access: "read", mode: "build" },
  });
  check("a chat we start there is stamped as us", started.author, spencer.id);

  // ---- the negative half ----
  const boxPeersAfter = await box.request("peer.list");
  check("the box still has no peer record for us", boxPeersAfter.peers.length, 0);

  const boxWs = await box.request("workspace.list");
  check(
    "our private project never reached the box",
    boxWs.workspaces.some((w) => w.name === "My private work"),
    false,
  );
  check(
    "and the box's catalog is only its own",
    boxWs.workspaces.length,
    catalog.workspaces.length,
  );

  const onDisk = JSON.parse(readFileSync(join(dir, "projects.json"), "utf8"));
  const theirIds = new Set(catalog.workspaces.map((w) => w.id));
  check(
    "nothing of theirs was written to our store",
    (onDisk.workspaces ?? []).some((w) => theirIds.has(w.id)),
    false,
  );
  const myPeers = await me.request("peer.list");
  check("we hold no peer record for the box", myPeers.peers.length, 0);

  // ---- capability scoping is enforced on THEIR side ----
  let denied = null;
  try {
    await me.request("term.open", { projectId, machineId: server.machineId });
  } catch (e) {
    denied = e.message;
  }
  checkTrue("a grant they withheld is refused (terminal)", !!denied);

  me.close();
  box.close();
} finally {
  desktop.kill("SIGKILL");
  await sleep(300);
  rmSync(dir, { recursive: true, force: true });
}

console.log(`\n${failures === 0 ? "PASS" : `FAIL — ${failures} check(s)`}`);
process.exit(failures === 0 ? 0 : 1);
