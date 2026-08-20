#!/usr/bin/env node
// Stand up a local desktop instance that is a guest on the real remote box, and
// leave it running so the merged sidebar can be looked at.
//
//   node scripts/demo-remote-guest.mjs <ip> <tunnelPort> <masterToken>

import { spawn } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connect } from "node:net";

const [IP, TUNNEL, MASTER] = process.argv.slice(2);
const BIN = join(import.meta.dirname, "..", "src-tauri", "target", "release", "threadknot-headless");
const PORT = 42895;
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
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
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
  throw new Error(`nothing on ${port}`);
}

const dir = mkdtempSync(join(tmpdir(), "threadknot-guest-demo-"));
const desktop = spawn(BIN, [], {
  env: { ...process.env, THREADKNOT_DATA_DIR: dir, THREADKNOT_PORT: String(PORT) },
  stdio: ["ignore", "pipe", "pipe"],
});
const token = await new Promise((resolve, reject) => {
  let buf = "";
  const on = (c) => {
    buf += c.toString();
    const m = buf.match(/token=([A-Za-z0-9_-]+)/);
    if (m) resolve(m[1]);
  };
  desktop.stdout.on("data", on);
  desktop.stderr.on("data", on);
  setTimeout(() => reject(new Error("no token")), 25000);
});
await listening(PORT);

const box = new Client(`ws://127.0.0.1:${TUNNEL}/ws?token=${MASTER}`);
await box.ready;
await box.request("device.rename", { name: "dev-box" });

const settings = { model: "sonnet", access: "read", mode: "build" };
const named = async (c, projectId, title, machineId) => {
  const t = await c.request("thread.create", {
    projectId,
    agent: "claude",
    settings,
    ...(machineId ? { machineId } : {}),
  });
  await c.request("thread.rename", {
    threadId: t.id,
    title,
    ...(machineId ? { machineId } : {}),
  });
  return t;
};

// Client work on the box.
const projects = {};
for (const [name, titles] of [
  ["Acme storefront", ["Checkout refactor", "Stripe webhook retries"]],
  ["Northwind site", ["Nav dropdown on mobile"]],
]) {
  const p = await box.request("project.create", { path: "/data", name });
  const id = p.id ?? p.project?.id;
  projects[name] = id;
  for (const t of titles) await named(box, id, t);
}
const spencer = await box.request("person.create", { name: "Spencer" });
await box.request("person.update", { personId: spencer.id, color: "#e0a34c" });

// The desktop's own private work.
const me = new Client(`ws://127.0.0.1:${PORT}/ws?token=${token}`);
await me.ready;
const mine = await me.request("project.create", { path: dir, name: "My side project" });
await named(me, mine.id ?? mine.project?.id, "Weekend rewrite");

// The guest link, over the public IP, with a one-time code.
const qr = await box.request("mobile.pair.begin", {
  capabilities: ["threads", "files", "git", "mesh"],
});
const added = await me.request("server.add", {
  origin: `http://${IP}:42800`,
  pairingCode: qr.code,
  deviceName: "Spencer's desktop",
});
await box.request("device.setPerson", { deviceId: added.deviceId, personId: spencer.id });

let server = null;
for (let i = 0; i < 60; i++) {
  await me.request("server.catalog", { serverId: added.id }).catch(() => {});
  const { servers } = await me.request("server.list");
  server = servers.find((s) => s.id === added.id);
  if (server?.online && server.personId) break;
  await sleep(500);
}
if (!server?.online) throw new Error("link never came up");

// One chat started from here, running over there.
await named(me, projects["Acme storefront"], "Driven from my desktop", server.machineId);

console.log(`\nREADY  desktop: http://127.0.0.1:${PORT}/?token=${token}`);
console.log(`       guest on ${server.name} as ${server.personName}`);
console.log(`dir    ${dir}\n`);
process.on("SIGINT", () => {
  desktop.kill("SIGTERM");
  process.exit(0);
});
await new Promise(() => {});
