#!/usr/bin/env node
// The last unverified claim: a real agent turn, running on a remote box, seen
// LIVE from a desktop that is only a guest on it.
//
// Everything else about the guest link has been proven against this box
// already. What has not is the event relay — the link forwards the server's own
// event frames to local clients with an origin tag, and until an actual turn
// runs there is no way to know a streaming reply renders on the guest side
// rather than only appearing after a refresh.
//
//   node scripts/test-remote-turn.mjs <ip|origin> <tunnelPort> <masterToken> [boxPath]

import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connect } from "node:net";

const [TARGET, TUNNEL, MASTER, BOX_PATH = "/srv/clients"] = process.argv.slice(2);
// A bare IP means the box's own port; a full origin means the relay.
const ORIGIN = TARGET.startsWith("http") ? TARGET.replace(/\/$/, "") : `http://${TARGET}:42800`;
const BIN = join(import.meta.dirname, "..", "src-tauri", "target", "release", "threadknot-headless");
const PORT = 42895;
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
  constructor(url, onFrame) {
    this.ws = new WebSocket(url);
    this.pending = new Map();
    this.id = 0;
    this.ready = new Promise((res, rej) => {
      this.ws.addEventListener("open", () => res());
      this.ws.addEventListener("error", (e) => rej(new Error(String(e.message ?? e))));
    });
    this.ws.addEventListener("message", (ev) => {
      const f = JSON.parse(ev.data);
      if (f.type === "response") {
        const slot = this.pending.get(f.id);
        if (!slot) return;
        this.pending.delete(f.id);
        f.ok ? slot.resolve(f.data) : slot.reject(new Error(f.error ?? "failed"));
        return;
      }
      onFrame?.(f);
    });
  }
  request(kind, payload = {}) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, type: kind, payload }));
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      setTimeout(() => {
        if (this.pending.delete(id)) reject(new Error(`${kind} timed out`));
      }, 120000);
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
  throw new Error(`nothing on ${port}`);
}

const dir = mkdtempSync(join(tmpdir(), "threadknot-turn-test-"));
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

try {
  await listening(PORT);
  await listening(Number(TUNNEL));

  const box = new Client(`ws://127.0.0.1:${TUNNEL}/ws?token=${MASTER}`);
  await box.ready;

  const agents = (await box.request("hello")).agents;
  check(
    "the box can run claude now",
    agents.find((a) => a.id === "claude")?.available,
    true,
  );

  const proj = await box.request("project.create", { path: BOX_PATH, name: "Turn test" });
  const projectId = proj.id ?? proj.project?.id;
  const spencer = await box.request("person.create", { name: "Spencer" });

  // Everything the guest's socket hears, so the relay can be measured rather
  // than assumed.
  const seen = { deltas: 0, persisted: 0, origins: new Set(), text: "" };
  const me = new Client(`ws://127.0.0.1:${PORT}/ws?token=${token}`, (f) => {
    if (f.type !== "event") return;
    if (f.origin) seen.origins.add(f.origin);
    if (f.seq < 0) seen.deltas++;
    else seen.persisted++;
    // `assistant_delta` is the streamed fragment, `assistant_message` the
    // settled turn (see AgentEvent in protocol.rs).
    const kind = f.event?.kind;
    if (kind === "assistant_delta" || kind === "assistant_message") {
      seen.text += f.event.text ?? "";
    }
  });
  await me.ready;

  const qr = await box.request("mobile.pair.begin", {
    capabilities: ["threads", "files", "git", "mesh"],
  });
  const added = await me.request("server.add", {
    origin: ORIGIN,
    pairingCode: qr.code,
    deviceName: "Turn test desktop",
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
  checkTrue("the guest link is up", !!server?.online);

  // A thread on the box, started from here, driven from here.
  const thread = await me.request("thread.create", {
    projectId,
    machineId: server.machineId,
    agent: "claude",
    settings: { model: "sonnet", access: "read", mode: "build" },
  });
  check("the thread is stamped as us", thread.author, spencer.id);

  // Watching it is what makes the server send deltas to this socket.
  await me.request("thread.get", { threadId: thread.id, machineId: server.machineId });

  console.log("        starting a real turn on the box…");
  await me.request("turn.start", {
    threadId: thread.id,
    machineId: server.machineId,
    text: "Reply with exactly the word: PONG. Nothing else.",
  });

  // Wait for the turn to finish, on the box's own record.
  let status = null;
  for (let i = 0; i < 120; i++) {
    await sleep(1000);
    const t = await box.request("thread.get", { threadId: thread.id });
    status = t.thread.status;
    if (status === "idle" && (t.events ?? []).some((e) => e.event?.kind === "turn_completed")) break;
  }
  check("the turn completed on the box", status, "idle");

  const final = await box.request("thread.get", { threadId: thread.id });
  const said = (final.events ?? [])
    .filter((e) => e.event?.kind === "assistant_message")
    .map((e) => e.event.text)
    .join("");
  checkTrue(`the agent actually answered (${JSON.stringify(said.trim().slice(0, 40))})`, said.length > 0);
  checkTrue("…and it ran on the box's own record", (final.events ?? []).length > 1);

  // The relay: did the guest see it happen, live?
  checkTrue(`the guest received events (${seen.persisted} persisted, ${seen.deltas} delta)`, seen.persisted > 0);
  check("…tagged with the box as their origin", [...seen.origins], [server.machineId]);
  checkTrue("…including streamed deltas, not just the final state", seen.deltas > 0);
  checkTrue(
    `…and the guest saw the reply text itself (${JSON.stringify(seen.text.trim().slice(0, 20))})`,
    seen.text.trim().length > 0,
  );

  me.close();
  box.close();
} finally {
  desktop.kill("SIGKILL");
  await sleep(300);
  rmSync(dir, { recursive: true, force: true });
}

console.log(`\n${failures === 0 ? "PASS" : `FAIL — ${failures} check(s)`}`);
process.exit(failures === 0 ? 0 : 1);
