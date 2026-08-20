#!/usr/bin/env node
// Smoke test for shared-machine people, driven over the real `/ws` against a
// real `threadknot-headless`. The Rust tests call `handle_request` directly;
// this one proves the same behaviour survives the socket, the token gate and
// the JSON on the wire — including that a second person's *credential*, not
// just a synthesized principal, sees its own sidebar.
//
// Usage: node scripts/smoke-people.mjs [path-to-threadknot-headless]

import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connect } from "node:net";

const BIN =
  process.argv[2] ?? join(import.meta.dirname, "..", "src-tauri", "target", "debug", "threadknot-headless");
const DATA_DIR = mkdtempSync(join(tmpdir(), "threadknot-smoke-people-"));
const PORT = 42873;

let failures = 0;
const check = (label, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  if (!ok) failures++;
  console.log(`${ok ? "  ok  " : "  FAIL"}  ${label}${ok ? "" : `\n          expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`}`);
};
const checkTrue = (label, actual) => check(label, !!actual, true);

/** One authenticated socket, with request/response correlated by id. */
class Client {
  constructor(url) {
    this.ws = new WebSocket(url);
    this.pending = new Map();
    this.id = 0;
    this.ready = new Promise((resolve, reject) => {
      this.ws.addEventListener("open", () => resolve());
      this.ws.addEventListener("error", (e) => reject(new Error(`socket error: ${e.message ?? e}`)));
    });
    this.ws.addEventListener("message", (ev) => {
      const frame = JSON.parse(ev.data);
      if (frame.type !== "response") return;
      const slot = this.pending.get(frame.id);
      if (!slot) return;
      this.pending.delete(frame.id);
      if (frame.ok) slot.resolve(frame.data);
      else slot.reject(new Error(frame.error ?? "request failed"));
    });
  }
  request(kind, payload = {}) {
    const id = ++this.id;
    // The wire field is `type`, not `kind` (see `ClientRequest`).
    this.ws.send(JSON.stringify({ id, type: kind, payload }));
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      setTimeout(() => {
        if (this.pending.delete(id)) reject(new Error(`${kind} timed out`));
      }, 15000);
    });
  }
  close() {
    this.ws.close();
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** The token is printed slightly before the listener is accepting, and after a
 *  restart the old process may still be releasing the port. Poll a cheap
 *  endpoint rather than racing it. */
async function waitForListening(port) {
  for (let i = 0; i < 60; i++) {
    const open = await new Promise((resolve) => {
      const sock = connect({ port, host: "127.0.0.1" });
      sock.once("connect", () => {
        sock.destroy();
        resolve(true);
      });
      sock.once("error", () => resolve(false));
    });
    if (open) return;
    await sleep(250);
  }
  throw new Error("server never started listening");
}

async function waitForToken(child) {
  // The headless binary prints its LAN URL with the token in it.
  return new Promise((resolve, reject) => {
    let buf = "";
    const onData = (chunk) => {
      buf += chunk.toString();
      const hit = buf.match(/token=([A-Za-z0-9_-]+)/);
      if (hit) resolve(hit[1]);
    };
    child.stdout.on("data", onData);
    child.stderr.on("data", onData);
    child.on("exit", (code) => reject(new Error(`server exited early (${code}): ${buf}`)));
    setTimeout(() => reject(new Error(`no token in output after 20s:\n${buf}`)), 20000);
  });
}

const server = spawn(BIN, [], {
  env: { ...process.env, THREADKNOT_DATA_DIR: DATA_DIR, THREADKNOT_PORT: String(PORT) },
  stdio: ["ignore", "pipe", "pipe"],
});

try {
  const token = await waitForToken(server);
  const base = `http://127.0.0.1:${PORT}`;
  await waitForListening(PORT);
  console.log(`server up on ${base}, data dir ${DATA_DIR}\n`);

  const owner = new Client(`ws://127.0.0.1:${PORT}/ws?token=${token}`);
  await owner.ready;

  // ---------------------------------------------------------------- owner ---
  const hello = await owner.request("hello");
  check("hello names the acting person", hello.person, "owner");

  const seeded = await owner.request("person.list");
  check("the owner is seeded", seeded.people.length, 1);
  check("…and it is who a master token is", seeded.acting, "owner");

  const project = await owner.request("project.create", { path: DATA_DIR });
  const projectId = project.id ?? project.project?.id;
  checkTrue("a project was created", !!projectId);

  const ownerThread = await owner.request("thread.create", {
    projectId,
    agent: "claude",
    settings: { model: "sonnet", access: "read", mode: "build" },
  });
  check("an owner's chat carries no author stamp", ownerThread.author, undefined);

  // --------------------------------------------------------------- intern ---
  const intern = await owner.request("person.create", { name: "Intern" });
  checkTrue("a second person was added", !!intern.id);

  // A REAL paired credential, redeemed over the real HTTP endpoint, then
  // pointed at the new person.
  const paired = await fetch(`${base}/api/mobile/pair`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token, deviceName: "Intern browser", platform: "web" }),
  }).then((r) => r.json());
  checkTrue("a device credential was minted", !!paired.credential);
  await owner.request("device.setPerson", {
    deviceId: paired.deviceId,
    personId: intern.id,
  });

  const internClient = new Client(`ws://127.0.0.1:${PORT}/ws?token=${paired.credential}`);
  await internClient.ready;

  const internHello = await internClient.request("hello");
  check("the device's socket acts as its person", internHello.person, intern.id);

  const internThread = await internClient.request("thread.create", {
    projectId,
    agent: "claude",
    settings: { model: "sonnet", access: "read", mode: "build" },
  });
  check("their chat is stamped with them", internThread.author, intern.id);

  // ------------------------------------------------------------ isolation ---
  const listFor = async (client) =>
    (await client.request("thread.list", { projectId })).threads;

  const ownerSees = await listFor(owner);
  const internSees = await listFor(internClient);
  check("both chats exist for both of them", [ownerSees.length, internSees.length], [2, 2]);
  check(
    "the author stamps survive the round trip",
    ownerSees.map((t) => t.author ?? "owner").sort(),
    ["owner", intern.id].sort(),
  );

  // Settling is the preference that used to be shared.
  await internClient.request("thread.setSettled", {
    threadId: ownerThread.id,
    settled: true,
  });
  const afterSettle = {
    intern: (await listFor(internClient)).find((t) => t.id === ownerThread.id),
    owner: (await listFor(owner)).find((t) => t.id === ownerThread.id),
  };
  checkTrue("the intern shelved it for themselves", !!afterSettle.intern.settledAt);
  check("…and not for the owner", afterSettle.owner.settledAt, undefined);

  // Stashing a workspace, the other one.
  const workspaces = (await owner.request("workspace.list")).workspaces;
  const wsId = workspaces[0]?.id;
  checkTrue("the project got a workspace", !!wsId);
  await internClient.request("workspace.setHidden", { workspaceId: wsId, hidden: true });
  const internWs = (await internClient.request("workspace.list")).workspaces.find((w) => w.id === wsId);
  const ownerWs = (await owner.request("workspace.list")).workspaces.find((w) => w.id === wsId);
  check("the intern stashed it for themselves", internWs.hidden, true);
  check("…and the owner's sidebar is untouched", ownerWs.hidden, undefined);

  // The owner's own toggle still writes the replicated record.
  await owner.request("workspace.setFavorite", { workspaceId: wsId, favorite: true });
  const ownerFav = (await owner.request("workspace.list")).workspaces.find((w) => w.id === wsId);
  check("an owner toggle still lands on the record", ownerFav.favorite, true);

  // ------------------------------------------------------------ authority ---
  let refused = null;
  try {
    await internClient.request("person.create", { name: "Sneaky" });
  } catch (e) {
    refused = e.message;
  }
  checkTrue("a device credential cannot add people", refused?.includes("master token"));

  // ------------------------------------------------- survives a restart ---
  owner.close();
  internClient.close();
  server.kill("SIGTERM");
  await sleep(1200);

  const restarted = spawn(BIN, [], {
    env: { ...process.env, THREADKNOT_DATA_DIR: DATA_DIR, THREADKNOT_PORT: String(PORT) },
    stdio: ["ignore", "pipe", "pipe"],
  });
  try {
    const token2 = await waitForToken(restarted);
    await waitForListening(PORT);
    const again = new Client(`ws://127.0.0.1:${PORT}/ws?token=${token2}`);
    await again.ready;
    const people = await again.request("person.list");
    check("people survive a restart", people.people.length, 2);

    const internAgain = new Client(`ws://127.0.0.1:${PORT}/ws?token=${paired.credential}`);
    await internAgain.ready;
    const stillShelved = (await internAgain.request("thread.list", { projectId })).threads.find(
      (t) => t.id === ownerThread.id,
    );
    checkTrue("…and so does their shelf", !!stillShelved.settledAt);
    const stillOwners = (await again.request("thread.list", { projectId })).threads.find(
      (t) => t.id === ownerThread.id,
    );
    check("…without leaking into the owner's", stillOwners.settledAt, undefined);
    again.close();
    internAgain.close();
  } finally {
    restarted.kill("SIGTERM");
  }
} finally {
  server.kill("SIGKILL");
  await sleep(300);
  rmSync(DATA_DIR, { recursive: true, force: true });
}

console.log(`\n${failures === 0 ? "PASS" : `FAIL — ${failures} check(s)`}`);
process.exit(failures === 0 ? 0 : 1);
