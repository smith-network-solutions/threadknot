#!/usr/bin/env node
// Two real Threadknots. One adds the other as a *server* (guest link), not as a
// peer, and this asserts the thing the design exists for: the link is one-way,
// and neither side's workspace catalog reaches the other's store.
//
// The positive checks (can I see their work, can I drive it, am I stamped as
// me) matter, but the negative ones are the point. A regression that turned
// this back into a peer link would still pass every positive check.
//
//   node scripts/smoke-servers.mjs

import { spawn } from "node:child_process";
import { mkdtempSync, rmSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connect } from "node:net";

const BIN = join(import.meta.dirname, "..", "src-tauri", "target", "debug", "threadknot-headless");
const SERVER_PORT = 42881; // "the team's shared box"
const DESKTOP_PORT = 42891; // "your laptop" (clear of the box's port+1/port+2 listeners)

let failures = 0;
const check = (label, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  if (!ok) failures++;
  console.log(
    `${ok ? "  ok  " : "  FAIL"}  ${label}` +
      (ok ? "" : `\n          expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`),
  );
};
const checkTrue = (label, actual) => check(label, !!actual, true);

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
      }, 20000);
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

function boot(port) {
  const dir = mkdtempSync(join(tmpdir(), `threadknot-smoke-srv-${port}-`));
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
    child.on("exit", (code) => reject(new Error(`:${port} exited (${code}): ${buf}`)));
    setTimeout(() => reject(new Error(`:${port} printed no token`)), 25000);
  });
  return { dir, child, token };
}

const box = boot(SERVER_PORT);
const laptop = boot(DESKTOP_PORT);

try {
  const boxToken = await box.token;
  const laptopToken = await laptop.token;
  await listening(SERVER_PORT);
  await listening(DESKTOP_PORT);
  console.log(`shared box  :${SERVER_PORT}  ${box.dir}`);
  console.log(`your laptop :${DESKTOP_PORT}  ${laptop.dir}\n`);

  const boxClient = new Client(`ws://127.0.0.1:${SERVER_PORT}/ws?token=${boxToken}`);
  const laptopClient = new Client(`ws://127.0.0.1:${DESKTOP_PORT}/ws?token=${laptopToken}`);
  await boxClient.ready;
  await laptopClient.ready;

  // ------------------------------------------------- set up both machines ---
  const boxProject = await boxClient.request("project.create", {
    path: box.dir,
    name: "Client site",
  });
  const boxProjectId = boxProject.id ?? boxProject.project?.id;
  await boxClient.request("thread.create", {
    projectId: boxProjectId,
    agent: "claude",
    settings: { model: "sonnet", access: "read", mode: "build" },
  });

  // Something private on the laptop, so "did anything leak the other way" has
  // a named subject rather than only a count.
  const mine = await laptopClient.request("project.create", {
    path: laptop.dir,
    name: "My private side project",
  });
  const myProjectId = mine.id ?? mine.project?.id;
  checkTrue("the laptop has a project of its own", !!myProjectId);

  // A person on the box, so the guest credential can be assigned to somebody.
  const spencer = await boxClient.request("person.create", { name: "Spencer" });

  // -------------------------------------------------------- the guest link ---
  const added = await laptopClient.request("server.add", {
    origin: `http://127.0.0.1:${SERVER_PORT}`,
    token: boxToken, // LAN convenience; over a relay this would be a pairing code
    deviceName: "Spencer's laptop",
  });
  checkTrue("the laptop added the box as a server", !!added.id);

  // The box's owner assigns that credential to a person, exactly as they would
  // a paired browser.
  await boxClient.request("device.setPerson", {
    deviceId: added.deviceId,
    personId: spencer.id,
  });

  // Wait for the outbound link to come up and re-read hello.
  let server = null;
  for (let i = 0; i < 40; i++) {
    const { servers } = await laptopClient.request("server.list");
    server = servers.find((s) => s.id === added.id);
    if (server?.online && server.machineId) break;
    await sleep(250);
  }
  checkTrue("the link came up", !!server?.online);
  checkTrue("…and learned the box's machine id", !!server?.machineId);
  check("…and who we are over there", server?.personId, spencer.id);

  // ----------------------------------------------------- we can see its work ---
  const catalog = await laptopClient.request("server.catalog", { serverId: added.id });
  checkTrue("the box's workspaces are readable", catalog.workspaces.length > 0);
  checkTrue(
    "…and its projects",
    catalog.projects.some((p) => p.id === boxProjectId),
  );

  const remoteThreads = await laptopClient.request("thread.list", {
    projectId: boxProjectId,
    machineId: server.machineId,
  });
  check("its chats route by machineId", remoteThreads.threads.length, 1);

  // ------------------------------------------- and drive it, stamped as us ---
  const started = await laptopClient.request("thread.create", {
    projectId: boxProjectId,
    machineId: server.machineId,
    agent: "claude",
    settings: { model: "sonnet", access: "read", mode: "build" },
  });
  check("a chat we start there is stamped with us", started.author, spencer.id);
  const onBox = await boxClient.request("thread.list", { projectId: boxProjectId });
  check("…and the box really has it", onBox.threads.length, 2);

  // =========================================================================
  // The negative half. This is what a peer link would fail.
  // =========================================================================

  // 1. The box never learned we exist as a machine.
  const boxPeers = await boxClient.request("peer.list");
  check("the box has no peer record for us", boxPeers.peers.length, 0);

  // 2. The box's workspace catalog did not grow our project.
  const boxWorkspaces = await boxClient.request("workspace.list");
  check(
    "our private project did not reach the box",
    boxWorkspaces.workspaces.some((w) => w.name === "My private side project"),
    false,
  );

  // 3. And ours did not grow theirs — the assertion the whole design turns on.
  const myWorkspaces = await laptopClient.request("workspace.list");
  check(
    "the box's workspaces are absent from our own list",
    myWorkspaces.workspaces.some((w) =>
      catalog.workspaces.some((their) => their.id === w.id),
    ),
    false,
  );

  // 4. Not merely absent from the response — absent from the FILE, which is
  //    what our own peers would be sent on their next connect.
  const onDisk = JSON.parse(readFileSync(join(laptop.dir, "projects.json"), "utf8"));
  const theirIds = new Set(catalog.workspaces.map((w) => w.id));
  check(
    "nothing of theirs was written to our store",
    (onDisk.workspaces ?? []).some((w) => theirIds.has(w.id)),
    false,
  );
  check(
    "…and none of their projects either",
    (onDisk.projects ?? []).some((p) => p.id === boxProjectId),
    false,
  );

  // 5. We have no peer record for them either, so nothing dials them as an equal.
  const myPeers = await laptopClient.request("peer.list");
  check("we hold no peer record for the box", myPeers.peers.length, 0);

  // 6. The credential is a device credential over there, not a peer one, so it
  //    shows up where its owner can scope and revoke it.
  const boxDevices = await boxClient.request("mobile.device.list");
  const ours = boxDevices.devices.find((d) => d.id === added.deviceId);
  checkTrue("the box sees us as a revocable device", !!ours);
  check("…assigned to the right person", ours?.personId, spencer.id);

  // 7. The stored record must not hand the credential to a client.
  const listed = (await laptopClient.request("server.list")).servers[0];
  check("the guest credential is not serialized to clients", "credential" in listed, false);

  // ------------------------------- sidebar prefs on somebody else's project ---
  //
  // Stashing one of their workspaces is a sidebar opinion, not an edit to their
  // record. It routes to them and lands in OUR person overlay over there, so
  // their own view is untouched — the guest link and the people overlay
  // composing is what makes this work at all.
  const theirWs = catalog.workspaces[0];
  await laptopClient.request("workspace.setHidden", {
    workspaceId: theirWs.id,
    hidden: true,
    machineId: server.machineId,
  });

  const asUs = await laptopClient.request("server.catalog", { serverId: added.id });
  check(
    "stashing their workspace works from here",
    asUs.workspaces.find((w) => w.id === theirWs.id)?.hidden,
    true,
  );
  const asThem = await boxClient.request("workspace.list");
  check(
    "…and their own sidebar is untouched",
    asThem.workspaces.find((w) => w.id === theirWs.id)?.hidden,
    undefined,
  );

  // ------------------------------------------------- revocation and removal ---
  await boxClient.request("mobile.device.revoke", { deviceId: added.deviceId });
  let refused = null;
  for (let i = 0; i < 20; i++) {
    await sleep(250);
    try {
      await laptopClient.request("thread.list", {
        projectId: boxProjectId,
        machineId: server.machineId,
      });
    } catch (e) {
      refused = e.message;
      break;
    }
  }
  checkTrue("revoking the device cuts us off", !!refused);

  await laptopClient.request("server.remove", { serverId: added.id });
  const after = await laptopClient.request("server.list");
  check("removing the server leaves no record", after.servers.length, 0);
  checkTrue("…and servers.json is the only thing that held it", existsSync(join(laptop.dir, "servers.json")));

  boxClient.close();
  laptopClient.close();
} finally {
  box.child.kill("SIGKILL");
  laptop.child.kill("SIGKILL");
  await sleep(300);
  rmSync(box.dir, { recursive: true, force: true });
  rmSync(laptop.dir, { recursive: true, force: true });
}

console.log(`\n${failures === 0 ? "PASS" : `FAIL — ${failures} check(s)`}`);
process.exit(failures === 0 ? 0 : 1);
