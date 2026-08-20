#!/usr/bin/env node
// Creating a workspace ON a server you are a guest of.
//
// The interesting half is the negative: `project.create` with a machineId
// already existed for MESH peers, and it does the opposite of what a guest link
// needs — it wraps the remote root in a LOCAL workspace record and replicates
// that record out to our own peers. Routing it to a server has to create the
// workspace in THEIR store and leave ours untouched, or the guest link leaks
// exactly what it was built to contain.
//
//   node scripts/test-remote-workspace.mjs <ip|origin> <tunnelPort> <masterToken> [boxPath]

import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connect } from "node:net";

const [TARGET, TUNNEL, MASTER, BOX_PATH = "/srv/clients/hermes-agents"] = process.argv.slice(2);
if (!TARGET || !TUNNEL || !MASTER) {
  console.error("usage: test-remote-workspace.mjs <ip|origin> <tunnelPort> <masterToken> [boxPath]");
  process.exit(2);
}
const ORIGIN = TARGET.startsWith("http") ? TARGET.replace(/\/$/, "") : `http://${TARGET}:42800`;
const BIN = join(import.meta.dirname, "..", "src-tauri", "target", "release", "threadknot-headless");
const PORT = 42897;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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

const dir = mkdtempSync(join(tmpdir(), "threadknot-ws-test-"));
const desktop = spawn(BIN, [], {
  env: { ...process.env, THREADKNOT_DATA_DIR: dir, THREADKNOT_PORT: String(PORT) },
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

const NAME = "Made from the desktop";
let createdId = null;

try {
  await listening(PORT);
  await listening(Number(TUNNEL));

  const box = new Client(`ws://127.0.0.1:${TUNNEL}/ws?token=${MASTER}`);
  await box.ready;
  const me = new Client(`ws://127.0.0.1:${PORT}/ws?token=${desktopToken}`);
  await me.ready;

  const before = (await box.request("workspace.list")).workspaces.length;

  const qr = await box.request("mobile.pair.begin", {
    capabilities: ["threads", "files", "git", "mesh"],
  });
  const added = await me.request("server.add", {
    origin: ORIGIN,
    pairingCode: qr.code,
    deviceName: "Workspace test desktop",
  });
  let server = null;
  for (let i = 0; i < 60; i++) {
    await me.request("server.catalog", { serverId: added.id }).catch(() => {});
    const { servers } = await me.request("server.list");
    server = servers.find((s) => s.id === added.id);
    if (server?.online && server.machineId) break;
    await sleep(500);
  }
  checkTrue("the guest link is up", !!server?.online);

  // ---- the thing under test ----
  const created = await me.request("project.create", {
    path: `${BOX_PATH}/tools`,
    name: NAME,
    machineId: server.machineId,
  });
  createdId = created.id ?? created.project?.id;
  checkTrue("the desktop could create a workspace over there", !!createdId);

  // ---- it exists on THEIR side ----
  const boxWs = (await box.request("workspace.list")).workspaces;
  check("the box gained exactly one workspace", boxWs.length, before + 1);
  checkTrue(
    "…and it is the one we asked for",
    boxWs.some((w) => w.name === NAME),
  );
  const boxProjects = (await box.request("project.list")).projects;
  const mine = boxProjects.find((p) => p.id === createdId);
  check("…rooted at the path we chose, on their disk", mine?.path, `${BOX_PATH}/tools`);

  // ---- and NOT on ours: the whole point ----
  const myWs = (await me.request("workspace.list")).workspaces;
  check(
    "our own workspace list is untouched",
    myWs.some((w) => w.name === NAME),
    false,
  );
  // A desktop that has only ever been a guest never writes projects.json at
  // all, so "no file" is the strongest form of this passing — not an error.
  let onDisk = null;
  try {
    onDisk = JSON.parse(readFileSync(join(dir, "projects.json"), "utf8"));
  } catch (e) {
    if (e.code !== "ENOENT") throw e;
  }
  check(
    onDisk
      ? "nothing was written to our store on disk"
      : "our store on disk was never even created",
    (onDisk?.workspaces ?? []).some((w) => w.name === NAME),
    false,
  );
  check(
    "and no project record either",
    (onDisk?.projects ?? []).some((p) => p.id === createdId),
    false,
  );

  // ---- but we can see and use it, through the catalog ----
  const catalog = await me.request("server.catalog", { serverId: added.id });
  checkTrue(
    "it shows up in their catalog, which is where the sidebar reads it",
    catalog.workspaces.some((w) => w.name === NAME),
  );
  const thread = await me.request("thread.create", {
    projectId: createdId,
    machineId: server.machineId,
    agent: "claude",
    settings: { model: "sonnet", access: "read", mode: "build" },
  });
  checkTrue("and a chat can be started in it from here", !!thread.id);

  me.close();
  box.close();
} finally {
  // Leave the box as we found it.
  if (createdId) {
    const cleanup = new Client(`ws://127.0.0.1:${TUNNEL}/ws?token=${MASTER}`);
    await cleanup.ready.catch(() => undefined);
    await cleanup.request("project.delete", { projectId: createdId }).catch(() => undefined);
    cleanup.close();
  }
  desktop.kill("SIGKILL");
  await sleep(300);
  rmSync(dir, { recursive: true, force: true });
}

console.log(`\n${failures === 0 ? "PASS" : `FAIL — ${failures} check(s)`}`);
process.exit(failures === 0 ? 0 : 1);
