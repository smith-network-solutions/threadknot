#!/usr/bin/env node
// Stand up a throwaway Threadknot with three people and a few chats each, so the
// sidebar's people row can be looked at rather than only asserted about.
// Prints the URL to open. Ctrl-C to stop; the data dir is left behind on
// purpose so the instance can be restarted against it.
//
//   node scripts/seed-people-demo.mjs [port] [dataDir]

import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connect } from "node:net";

const PORT = Number(process.argv[2] ?? 42874);
const DATA_DIR = process.argv[3] ?? mkdtempSync(join(tmpdir(), "threadknot-demo-people-"));
const BIN = join(import.meta.dirname, "..", "src-tauri", "target", "debug", "threadknot-headless");
const WORK = join(DATA_DIR, "storefront");
mkdirSync(WORK, { recursive: true });

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
  throw new Error("server never came up");
}

const server = spawn(BIN, [], {
  env: { ...process.env, THREADKNOT_DATA_DIR: DATA_DIR, THREADKNOT_PORT: String(PORT) },
  stdio: ["ignore", "pipe", "pipe"],
});
const token = await new Promise((resolve, reject) => {
  let buf = "";
  const on = (c) => {
    buf += c.toString();
    const m = buf.match(/token=([A-Za-z0-9_-]+)/);
    if (m) resolve(m[1]);
  };
  server.stdout.on("data", on);
  server.stderr.on("data", on);
  setTimeout(() => reject(new Error(`no token:\n${buf}`)), 20000);
});
await listening(PORT);

const base = `http://127.0.0.1:${PORT}`;
const owner = new Client(`ws://127.0.0.1:${PORT}/ws?token=${token}`);
await owner.ready;

const project = await owner.request("project.create", { path: WORK, name: "Storefront" });
const projectId = project.id ?? project.project?.id;

const settings = { model: "sonnet", access: "read", mode: "build" };
const named = async (client, title) => {
  const t = await client.request("thread.create", { projectId, agent: "claude", settings });
  await client.request("thread.rename", { threadId: t.id, title });
  return t;
};

await owner.request("person.update", { personId: "owner", name: "Spencer", color: "#e0a34c" });
await named(owner, "Checkout refactor");
await named(owner, "Docker compose for staging");

for (const [name, color, titles] of [
  ["Dani", "#43c9a5", ["Fix product image CDN", "Nav dropdown on mobile"]],
  ["Marco", "#7aa2f7", ["Stripe webhook retries"]],
]) {
  const person = await owner.request("person.create", { name });
  await owner.request("person.update", { personId: person.id, color });
  const paired = await fetch(`${base}/api/mobile/pair`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token, deviceName: `${name}'s browser`, platform: "web" }),
  }).then((r) => r.json());
  await owner.request("device.setPerson", { deviceId: paired.deviceId, personId: person.id });
  const client = new Client(`ws://127.0.0.1:${PORT}/ws?token=${paired.credential}`);
  await client.ready;
  for (const title of titles) await named(client, title);
}

console.log(`\nREADY  ${base}/?token=${token}\ndata dir ${DATA_DIR}\n`);
process.on("SIGINT", () => {
  server.kill("SIGTERM");
  process.exit(0);
});
await new Promise(() => {});
