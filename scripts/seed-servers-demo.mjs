#!/usr/bin/env node
// Two throwaway Threadknots: a "shared box" with client work on it, and a
// "laptop" that is a guest on it. Prints the laptop's URL so the merged sidebar
// can be looked at. Ctrl-C stops both.
//
//   node scripts/seed-servers-demo.mjs

import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connect } from "node:net";

const BIN = join(import.meta.dirname, "..", "src-tauri", "target", "debug", "threadknot-headless");
const BOX = 42883;
const LAPTOP = 42893;
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

function boot(port, label) {
  const dir = mkdtempSync(join(tmpdir(), `threadknot-demo-${label}-`));
  const child = spawn(BIN, [], {
    env: { ...process.env, THREADKNOT_DATA_DIR: dir, THREADKNOT_PORT: String(port) },
    stdio: ["ignore", "pipe", "pipe"],
  });
  const token = new Promise((resolve, reject) => {
    let buf = "";
    const on = (c) => {
      buf += c.toString();
      const m = buf.match(/token=([A-Za-z0-9_-]+)/);
      if (m) resolve(m[1]);
    };
    child.stdout.on("data", on);
    child.stderr.on("data", on);
    setTimeout(() => reject(new Error(`${label}: no token`)), 25000);
  });
  return { dir, child, token };
}

const box = boot(BOX, "box");
const laptop = boot(LAPTOP, "laptop");
const boxToken = await box.token;
const laptopToken = await laptop.token;
await listening(BOX);
await listening(LAPTOP);

const b = new Client(`ws://127.0.0.1:${BOX}/ws?token=${boxToken}`);
const l = new Client(`ws://127.0.0.1:${LAPTOP}/ws?token=${laptopToken}`);
await b.ready;
await l.ready;

const settings = { model: "sonnet", access: "read", mode: "build" };
const named = async (client, projectId, title, machineId) => {
  const t = await client.request("thread.create", {
    projectId,
    agent: "claude",
    settings,
    ...(machineId ? { machineId } : {}),
  });
  await client.request("thread.rename", {
    threadId: t.id,
    title,
    ...(machineId ? { machineId } : {}),
  });
  return t;
};

// The shared box: two client sites, people, work in flight.
await b.request("device.rename", { name: "dev-box" });
const spencer = await b.request("person.create", { name: "Spencer" });
await b.request("person.update", { personId: spencer.id, color: "#e0a34c" });
const dani = await b.request("person.create", { name: "Dani" });
await b.request("person.update", { personId: dani.id, color: "#43c9a5" });

for (const [name, titles] of [
  ["Acme storefront", ["Checkout refactor", "Stripe webhook retries"]],
  ["Northwind site", ["Nav dropdown on mobile"]],
]) {
  const dir = join(box.dir, name.replace(/\W+/g, "-"));
  mkdirSync(dir, { recursive: true });
  const p = await b.request("project.create", { path: dir, name });
  for (const t of titles) await named(b, p.id ?? p.project?.id, t);
}

// The laptop: its own private work, which must never reach the box.
const mineDir = join(laptop.dir, "side-project");
mkdirSync(mineDir, { recursive: true });
const mine = await l.request("project.create", { path: mineDir, name: "My side project" });
await named(l, mine.id ?? mine.project?.id, "Weekend rewrite");

// The guest link.
const added = await l.request("server.add", {
  origin: `http://127.0.0.1:${BOX}`,
  token: boxToken,
  deviceName: "Spencer's laptop",
});
await b.request("device.setPerson", { deviceId: added.deviceId, personId: spencer.id });

let server = null;
for (let i = 0; i < 40; i++) {
  const { servers } = await l.request("server.list");
  server = servers.find((s) => s.id === added.id);
  if (server?.online && server.machineId) break;
  await sleep(250);
}
if (!server?.online) throw new Error("link never came up");

// A chat started from the laptop, on the box, stamped as Spencer.
const catalog = await l.request("server.catalog", { serverId: added.id });
const acme = catalog.projects.find((p) => p.name === "Acme storefront");
await named(l, acme.id, "Driven from my laptop", server.machineId);

console.log(`\nREADY  laptop: http://127.0.0.1:${LAPTOP}/?token=${laptopToken}`);
console.log(`       box:    http://127.0.0.1:${BOX}/?token=${boxToken}`);
console.log(`dirs   ${laptop.dir}  ${box.dir}\n`);

process.on("SIGINT", () => {
  box.child.kill("SIGTERM");
  laptop.child.kill("SIGTERM");
  process.exit(0);
});
await new Promise(() => {});
