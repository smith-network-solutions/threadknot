#!/usr/bin/env node
// teams-cdp.mjs — Bosun lookout: a 1:1 Microsoft Teams chat, read out of the
// running Teams for Linux app over the Chrome DevTools Protocol.
//
// Contract (docs/BOSUN.md §4): reads BOSUN_WATERMARK (an ISO 8601 timestamp,
// the arrival time of the newest message already seen), prints one NDJSON
// signal per burst of new messages, then {"watermark": "<newest ISO time>"}.
//
//   First run (empty BOSUN_WATERMARK): prints only a watermark (the newest
//   message's time right now). No signals, so switching it on never dumps the
//   backlog on the Bosun.
//   Later runs: reads the message cache, groups messages newer than the
//   watermark into bursts (a run of messages <=10 min apart), and emits one
//   signal per burst that contains at least one message from the contact
//   (unless TEAMS_INCLUDE_MINE_ONLY=1). The watermark advances to the newest
//   message regardless of filtering, so mine-only tails are not re-scanned.
//
// HOW IT READS (discovered, see lookouts/README.md): Teams for Linux is a
// signed-in Electron app. This attaches to its "Microsoft Teams" page target
// over the CDP endpoint and runs code in the page with Runtime.evaluate. The
// message history is read straight from the client's own IndexedDB caches —
// `conversation-manager` (to resolve the contact to a 1:1 conversation) and
// `replychain-manager` (message content) — which is entirely read-only and
// sends NO read receipts. Attachments/inline images are downloaded from the
// page context with fetch(url,{credentials:"include"}) (the app's own cookies
// authorize them) and transferred out as base64.
//
// Env:
//   TEAMS_CDP_URL            CDP base (default http://127.0.0.1:9222)
//   TEAMS_CONTACT            display name of the other party (default "William Hunt")
//   TEAMS_WORKSPACE_HINT     optional; copied to hints.workspace on every signal
//   TEAMS_MAX_MESSAGES       max new messages considered per run (default 200, newest first)
//   TEAMS_INCLUDE_MINE_ONLY  1 = also emit bursts that contain only your own messages
//   BOSUN_WATERMARK, BOSUN_STATE_DIR   set by Threadknot
//
// State (BOSUN_STATE_DIR): attachments/<messageId>/<filename> for saved media.
// Read-only: never sends a message, never marks read, never navigates or
// changes any client state. Only reads IndexedDB and GETs media the app cached.
//
// Exit codes: 0 ok (including "nothing new"); 3 Teams not running / CDP port
// closed (no watermark printed, so the run retries); 2 signed out (no Teams
// page / no message cache).
//
// By hand:
//   BOSUN_WATERMARK= ./lookouts/teams-cdp.mjs | jq                    # seed
//   BOSUN_WATERMARK=2026-09-26T00:00:00Z ./lookouts/teams-cdp.mjs | jq
//   ./lookouts/teams-cdp.mjs --dump --days 3                          # markdown history
//   ./lookouts/teams-cdp.mjs --dump --since 2026-09-27T00:00:00Z
//   ./lookouts/teams-cdp.mjs --selftest                              # offline checks

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const CDP_URL = (process.env.TEAMS_CDP_URL || "http://127.0.0.1:9222").replace(/\/+$/, "");
const CONTACT = process.env.TEAMS_CONTACT || "William Hunt";
const WORKSPACE_HINT = process.env.TEAMS_WORKSPACE_HINT || "";
const MAX_MESSAGES = Math.max(1, parseInt(process.env.TEAMS_MAX_MESSAGES || "200", 10) || 200);
const INCLUDE_MINE_ONLY = process.env.TEAMS_INCLUDE_MINE_ONLY === "1";
const BURST_GAP_MS = 10 * 60 * 1000;

function stateDir() {
  const d = process.env.BOSUN_STATE_DIR;
  if (d) return d;
  return fs.mkdtempSync(path.join(os.tmpdir(), "teams-cdp-"));
}
function log(...a) { process.stderr.write("teams-cdp: " + a.join(" ") + "\n"); }

// ------------------------- pure helpers (--selftest) -------------------------

// HTML entity decode for the handful Teams emits.
function decodeEntities(s) {
  return String(s)
    .replace(/&nbsp;/g, " ")
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&#(\d+);/g, (_, n) => String.fromCodePoint(parseInt(n, 10)))
    .replace(/&#x([0-9a-f]+);/gi, (_, n) => String.fromCodePoint(parseInt(n, 16)));
}

// Strip a Teams message's HTML to plain text. Keeps <a href> targets as URLs,
// turns <br>/<p>/<div> into newlines, and (when imgMap is given) replaces each
// <img> whose object id has a saved path with "[image: <path>]".
function htmlToText(html, imgMap) {
  if (html == null) return "";
  let s = String(html);
  // Plain Text messages carry no markup: keep their real newlines, just decode.
  if (!/<[a-z!/]/i.test(s)) return decodeEntities(s).replace(/\r\n/g, "\n").trim();
  // HTML messages: source whitespace (incl. newlines between tags) is
  // insignificant — collapse it so a "<br />\r\n" is a single line break.
  s = s.replace(/[\r\n\t]+/g, " ");
  // reply/quote blockquotes are pulled out separately; drop them here
  s = s.replace(/<blockquote[\s\S]*?<\/blockquote>/gi, "");
  // images -> placeholder
  s = s.replace(/<img\b[^>]*>/gi, (tag) => {
    const id = (tag.match(/itemid="([^"]+)"/i) || tag.match(/\/objects\/([^/"]+)\//i) || [])[1];
    const p = imgMap && id ? imgMap[id] : null;
    return p ? `[image: ${p}]` : "[image]";
  });
  // anchors -> "text (url)" when the url adds information
  s = s.replace(/<a\b[^>]*href="([^"]*)"[^>]*>([\s\S]*?)<\/a>/gi, (_, href, inner) => {
    const text = inner.replace(/<[^>]+>/g, "").trim();
    const url = decodeEntities(href).trim();
    if (!url) return text;
    if (!text || text === url) return url;
    return `${text} (${url})`;
  });
  s = s.replace(/<br\s*\/?>/gi, "\n");
  s = s.replace(/<\/(p|div|li|pre|blockquote|tr)>/gi, "\n");
  s = s.replace(/<li\b[^>]*>/gi, "- ");
  s = s.replace(/<[^>]+>/g, "");
  s = decodeEntities(s);
  s = s.replace(/\r\n/g, "\n").replace(/[ \t]+\n/g, "\n").replace(/\n[ \t]+/g, "\n").replace(/\n{3,}/g, "\n\n");
  return s.trim();
}

// Extract a reply/quote header, if any, from a Teams RichText/Html message.
// Returns "" or a "> [reply to <name>]: <preview>" line. mriName maps mri->name.
function extractReply(html, mriName) {
  const m = String(html || "").match(/<blockquote[^>]*schema\.skype\.com\/Reply[^>]*>([\s\S]*?)<\/blockquote>/i);
  if (!m) return "";
  const inner = m[1];
  const mri = (inner.match(/itemprop="mri"[^>]*itemid="([^"]+)"/i) || inner.match(/itemid="(8:[^"]+)"/i) || [])[1];
  const preview = htmlToText((inner.match(/itemprop="preview"[^>]*>([\s\S]*?)<\/p>/i) || [])[1] || "");
  const name = (mri && mriName && mriName[mri]) || "someone";
  const prev = preview ? `: ${preview}` : "";
  return `> [reply to ${name}${prev}]`;
}

// Group time-sorted messages into bursts (<=gapMs between neighbours).
function groupBursts(messages, gapMs) {
  const bursts = [];
  let cur = null;
  for (const m of messages) {
    if (cur && m.timeMs - cur[cur.length - 1].timeMs <= gapMs) cur.push(m);
    else { cur = [m]; bursts.push(cur); }
  }
  return bursts;
}

// watermark comparison: is message time strictly newer than watermark?
function isNewer(timeMs, watermark) {
  if (!watermark) return false;
  const w = Date.parse(watermark);
  if (Number.isNaN(w)) return true;
  return timeMs > w;
}

// filename + extension from a media URL and its content-type.
const CT_EXT = {
  "image/jpeg": "jpg", "image/jpg": "jpg", "image/png": "png", "image/gif": "gif",
  "image/webp": "webp", "image/heic": "heic", "image/bmp": "bmp",
  "application/pdf": "pdf", "text/plain": "txt", "application/zip": "zip",
  "application/octet-stream": "bin",
};
function deriveFilename(url, contentType, fallbackId) {
  let base = "";
  try {
    const u = new URL(url);
    const objMatch = u.pathname.match(/\/objects\/([^/]+)\//);
    if (objMatch) base = objMatch[1];
    else {
      const last = u.pathname.split("/").filter(Boolean).pop() || "";
      base = decodeURIComponent(last);
    }
  } catch { base = ""; }
  if (!base) base = String(fallbackId || "attachment");
  base = base.replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 120);
  const ct = (contentType || "").split(";")[0].trim().toLowerCase();
  const hasExt = /\.[A-Za-z0-9]{2,5}$/.test(base);
  if (hasExt) return base;
  const ext = CT_EXT[ct] || (ct.startsWith("image/") ? ct.slice(6) : "bin");
  return `${base}.${ext}`;
}

// Pull inline-image object URLs out of a message's HTML.
function imageUrlsFromHtml(html) {
  const out = [];
  const re = /<img\b[^>]*\bsrc="([^"]+)"[^>]*>/gi;
  let m;
  while ((m = re.exec(String(html || "")))) {
    const src = decodeEntities(m[1]);
    const id = (m[0].match(/itemid="([^"]+)"/i) || m[0].match(/\/objects\/([^/"]+)\//i) || [])[1] || src;
    out.push({ id, url: src });
  }
  return out;
}

// File attachments from message.properties.files (JSON string or array).
function fileAttachmentsFromMessage(msg) {
  const out = [];
  let files = msg && msg.properties && msg.properties.files;
  if (typeof files === "string") { try { files = JSON.parse(files); } catch { files = null; } }
  if (Array.isArray(files)) {
    for (const f of files) {
      const url = f.objectUrl || f.fileUrl || f.downloadUrl || f.itemUrl || f.filePreview || "";
      const name = f.fileName || f.title || f.name || "";
      if (url) out.push({ url, name });
    }
  }
  return out;
}

// -------------------------------- CDP client --------------------------------

class PortClosed extends Error {}
class SignedOut extends Error {}

async function listTargets() {
  let res;
  try {
    res = await fetch(CDP_URL + "/json/list", { signal: AbortSignal.timeout(8000) });
  } catch (e) {
    throw new PortClosed(`CDP port not answering at ${CDP_URL}: ${e.message}`);
  }
  if (!res.ok) throw new PortClosed(`CDP /json/list returned ${res.status}`);
  return res.json();
}

async function teamsPage() {
  const list = await listTargets();
  const p = list.find((t) => t.type === "page" && /teams\.(live|microsoft|cloud)/.test(t.url || t.title || ""));
  if (!p) throw new SignedOut("no Microsoft Teams page target (app signed out or not loaded)");
  return p;
}

function attach(wsUrl) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(wsUrl);
    let id = 0;
    const pending = new Map();
    const timer = setTimeout(() => reject(new PortClosed("CDP websocket connect timed out")), 8000);
    ws.onopen = () => {
      clearTimeout(timer);
      resolve({
        eval: async (expression) => {
          const mid = ++id;
          const result = await new Promise((res, rej) => {
            pending.set(mid, { res, rej });
            ws.send(JSON.stringify({
              id: mid, method: "Runtime.evaluate",
              params: { expression, awaitPromise: true, returnByValue: true },
            }));
          });
          if (result.exceptionDetails) {
            throw new Error("page eval failed: " + JSON.stringify(result.exceptionDetails).slice(0, 400));
          }
          return result.result.value;
        },
        close: () => ws.close(),
      });
    };
    ws.onerror = () => { clearTimeout(timer); reject(new PortClosed("CDP websocket error")); };
    ws.onmessage = (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id && pending.has(m.id)) {
        const { res, rej } = pending.get(m.id);
        pending.delete(m.id);
        if (m.error) rej(new Error(JSON.stringify(m.error)));
        else res(m.result);
      }
    };
  });
}

// ---- page-side programs (strings run via Runtime.evaluate) ----

// Resolve the contact display name to its mri and its most-recently-active 1:1
// conversation. Returns {conversationId, contactMri, contactName, participants}.
function pgResolve(contact) {
  const c = JSON.stringify(contact);
  return `(async () => {
    const dbs = (await indexedDB.databases()).map(d => d.name);
    const cname = dbs.find(n => /conversation-manager:/.test(n));
    if (!cname) return { signedOut: true };
    const open = n => new Promise((res, rej) => { const r = indexedDB.open(n); r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error); });
    const db = await open(cname);
    const convs = await new Promise((res) => { const g = db.transaction("conversations","readonly").objectStore("conversations").getAll(); g.onsuccess = () => res(g.result||[]); g.onerror = () => res([]); });
    db.close();
    const want = ${c}.toLowerCase();
    // map mri -> displayName from every chatTitle.avatarUsersInfo we can see
    const names = {};
    for (const cv of convs) {
      const ct = cv.chatTitle;
      if (ct && Array.isArray(ct.avatarUsersInfo)) for (const u of ct.avatarUsersInfo) if (u.mri && u.displayName) names[u.mri] = u.displayName;
    }
    let mri = null;
    for (const [m, dn] of Object.entries(names)) if (dn.toLowerCase() === want) { mri = m; break; }
    if (!mri) for (const [m, dn] of Object.entries(names)) if (dn.toLowerCase().includes(want)) { mri = m; break; }
    if (!mri) return { notFound: true, knownNames: Object.values(names).slice(0, 40) };
    // 1:1 conversations = exactly two members incl. the contact
    const oneToOne = convs.filter(cv => { const ms = (cv.members||[]).map(x => x.id||x.mri||x); return ms.length === 2 && ms.includes(mri); });
    oneToOne.sort((a,b) => (b.lastMessageTimeUtc||0) - (a.lastMessageTimeUtc||0));
    const target = oneToOne[0];
    if (!target) return { notFound: true, reason: "contact has no 1:1 conversation", contactMri: mri };
    const members = (target.members||[]).map(x => x.id||x.mri||x);
    return { conversationId: target.id, contactMri: mri, contactName: names[mri] || ${c}, participants: members, mriNames: names };
  })()`;
}

// Read messages for a conversation. Returns {messages:[{id,timeMs,mine,fromMri,fromName,messageType,content}]}
function pgMessages(conversationId, contactMri) {
  return `(async () => {
    const CID = ${JSON.stringify(conversationId)};
    const CONTACT = ${JSON.stringify(contactMri)};
    const dbs = (await indexedDB.databases()).map(d => d.name);
    const rname = dbs.find(n => /replychain-manager:/.test(n));
    if (!rname) return { signedOut: true };
    const open = n => new Promise((res, rej) => { const r = indexedDB.open(n); r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error); });
    const db = await open(rname);
    const chains = await new Promise((res) => { const g = db.transaction("replychains","readonly").objectStore("replychains").getAll(); g.onsuccess = () => res(g.result||[]); g.onerror = () => res([]); });
    db.close();
    const out = [];
    for (const ch of chains) {
      if (ch.conversationId !== CID) continue;
      const mm = ch.messageMap || {};
      for (const k of Object.keys(mm)) {
        const m = mm[k];
        if (!m) continue;
        if (m.deletionInfo) continue; // deleted / recalled
        const t = Number(m.originalArrivalTime || m.clientArrivalTime || m.composeTime || m.id || 0);
        if (!t) continue;
        const mt = m.messageType || m.messagetype || "Text";
        if (mt === "ThreadActivity" || /^ThreadActivity/.test(mt)) continue; // roster/system noise
        if (!(m.content && String(m.content).trim()) ) continue;
        const fromMri = m.creator || m.from || null;
        out.push({
          id: String(m.id),
          timeMs: t,
          mine: !!m.isSentByCurrentUser,
          fromMri,
          fromName: m.imDisplayName || m.imdisplayname || "",
          messageType: mt,
          content: String(m.content),
          hasFiles: !!(m.properties && m.properties.files && m.properties.files !== "[]"),
          filesRaw: (m.properties && m.properties.files) || null,
        });
      }
    }
    out.sort((a,b) => a.timeMs - b.timeMs);
    return { messages: out };
  })()`;
}

// Download one media URL in the page context; returns {ok,status,contentType,base64}.
function pgDownload(url) {
  return `(async () => {
    try {
      const r = await fetch(${JSON.stringify(url)}, { credentials: "include" });
      if (!r.ok) return { ok: false, status: r.status };
      const ct = r.headers.get("content-type") || "";
      const buf = new Uint8Array(await r.arrayBuffer());
      let bin = ""; const chunk = 0x8000;
      for (let i = 0; i < buf.length; i += chunk) bin += String.fromCharCode.apply(null, buf.subarray(i, i + chunk));
      return { ok: true, status: r.status, contentType: ct, base64: btoa(bin) };
    } catch (e) { return { ok: false, error: String(e).slice(0, 200) }; }
  })()`;
}

// -------------------------------- attachments --------------------------------

// Download every image + file attachment on a message, save under
// stateDir/attachments/<messageId>/, return { imgMap: {objId->path}, paths: [] }.
async function fetchAttachmentsForMessage(page, msg, dir) {
  const imgMap = {};
  const paths = [];
  const images = imageUrlsFromHtml(msg.content);
  const files = fileAttachmentsFromMessage(msg);
  const items = [
    ...images.map((i) => ({ id: i.id, url: i.url, name: null })),
    ...files.map((f) => ({ id: null, url: f.url, name: f.name })),
  ];
  if (!items.length) return { imgMap, paths };
  const msgDir = path.join(dir, "attachments", msg.id);
  for (const it of items) {
    let res;
    try { res = await page.eval(pgDownload(it.url)); }
    catch (e) { log(`attachment fetch error (${msg.id}): ${e.message}`); continue; }
    if (!res || !res.ok) {
      // a download that needs a state-changing call, or auth we don't have: record the URL, skip bytes
      log(`attachment not downloadable (${msg.id}, status ${res && res.status}); recording url`);
      paths.push(it.url);
      continue;
    }
    const filename = it.name && /\.[A-Za-z0-9]{2,5}$/.test(it.name)
      ? it.name.replace(/[^A-Za-z0-9._-]/g, "_")
      : deriveFilename(it.url, res.contentType, it.id);
    fs.mkdirSync(msgDir, { recursive: true });
    const full = path.join(msgDir, filename);
    fs.writeFileSync(full, Buffer.from(res.base64, "base64"));
    paths.push(full);
    if (it.id) imgMap[it.id] = full;
  }
  return { imgMap, paths };
}

// --------------------------------- rendering ---------------------------------

function hhmm(timeMs) {
  const d = new Date(timeMs);
  const h = String(d.getHours()).padStart(2, "0");
  const m = String(d.getMinutes()).padStart(2, "0");
  return `${h}:${m}`;
}

function renderMessageLine(msg, imgMap, mriNames) {
  const reply = extractReply(msg.content, mriNames);
  const text = htmlToText(msg.content, imgMap);
  const name = msg.mine ? "Me" : (msg.fromName || "Them");
  const lines = [];
  if (reply) lines.push(`[${hhmm(msg.timeMs)}] ${name} ${reply}`);
  const head = reply ? "" : `[${hhmm(msg.timeMs)}] ${name}: `;
  lines.push(head + (text || "").split("\n").join("\n    "));
  return lines.join("\n");
}

// --------------------------------- main modes --------------------------------

async function withPage(fn) {
  const target = await teamsPage();
  const page = await attach(target.webSocketDebuggerUrl);
  try { return await fn(page, target); }
  finally { page.close(); }
}

function conversationUrl(target, cid) {
  // deep link on whatever host the running client uses (teams.live.com for
  // personal Teams; teams.microsoft.com / teams.cloud.microsoft for work).
  let origin = "https://teams.live.com/v2/";
  try { const u = new URL(target.url); origin = u.origin + (u.pathname.startsWith("/v2") ? "/v2/" : "/"); } catch {}
  return `${origin}#/conversations/${encodeURIComponent(cid)}?ctx=chat`;
}

async function runPoll() {
  const watermark = process.env.BOSUN_WATERMARK || "";
  const dir = stateDir();
  await withPage(async (page, target) => {
    const resolved = await page.eval(pgResolve(CONTACT));
    if (resolved.signedOut) throw new SignedOut("Teams message cache absent (signed out)");
    if (resolved.notFound) {
      log(`contact "${CONTACT}" not found among cached conversations` + (resolved.knownNames ? ` (known: ${resolved.knownNames.slice(0,10).join(", ")})` : ""));
      // not fatal: print no watermark change, exit 0 (nothing to do)
      if (!watermark) process.stdout.write(JSON.stringify({ watermark: new Date().toISOString() }) + "\n");
      return;
    }
    const cid = resolved.conversationId;
    const mriNames = resolved.mriNames || {};
    const { messages } = await page.eval(pgMessages(cid, resolved.contactMri));
    if (!messages || !messages.length) {
      if (!watermark) process.stdout.write(JSON.stringify({ watermark: new Date().toISOString() }) + "\n");
      return;
    }
    const newestIso = new Date(messages[messages.length - 1].timeMs).toISOString();

    // Seed run: publish the watermark, emit nothing.
    if (!watermark) {
      process.stdout.write(JSON.stringify({ watermark: newestIso }) + "\n");
      return;
    }

    // New messages only, newest MAX_MESSAGES.
    let fresh = messages.filter((m) => isNewer(m.timeMs, watermark));
    if (fresh.length > MAX_MESSAGES) fresh = fresh.slice(fresh.length - MAX_MESSAGES);
    if (!fresh.length) {
      process.stdout.write(JSON.stringify({ watermark: newestIso }) + "\n");
      return;
    }

    const bursts = groupBursts(fresh, BURST_GAP_MS);
    for (const burst of bursts) {
      const fromContact = burst.filter((m) => !m.mine);
      if (!fromContact.length && !INCLUDE_MINE_ONLY) continue;

      // attachments first, so the body can reference saved paths inline
      const imgMap = {};
      const allPaths = [];
      for (const m of burst) {
        const { imgMap: im, paths } = await fetchAttachmentsForMessage(page, m, dir);
        Object.assign(imgMap, im);
        allPaths.push(...paths);
      }

      const first = (fromContact[0] || burst[0]);
      const firstText = htmlToText(first.content, imgMap).replace(/\n+/g, " ").trim();
      const last = burst[burst.length - 1];
      const body = burst.map((m) => renderMessageLine(m, imgMap, mriNames)).join("\n");

      const refs = {
        conversationId: cid,
        lastMessageId: last.id,
        attachments: allPaths.join(";"),
        url: conversationUrl(target, cid),
      };
      const signal = {
        id: `teams:${cid}:${last.id}`,
        kind: "teams",
        title: `Teams · Bill: ${firstText.slice(0, 70)}`,
        body,
        observedAt: new Date(last.timeMs).toISOString(),
        refs,
      };
      if (WORKSPACE_HINT) signal.hints = { workspace: WORKSPACE_HINT };
      process.stdout.write(JSON.stringify(signal) + "\n");
    }
    process.stdout.write(JSON.stringify({ watermark: newestIso }) + "\n");
  });
}

async function runDump(args) {
  const dir = stateDir();
  let sinceMs;
  const sinceIdx = args.indexOf("--since");
  if (sinceIdx >= 0 && args[sinceIdx + 1]) {
    sinceMs = Date.parse(args[sinceIdx + 1]);
    if (Number.isNaN(sinceMs)) { log("bad --since value"); process.exit(1); }
  } else {
    const daysIdx = args.indexOf("--days");
    const days = daysIdx >= 0 && args[daysIdx + 1] ? parseFloat(args[daysIdx + 1]) : 3;
    sinceMs = Date.now() - days * 86400000;
  }
  await withPage(async (page, target) => {
    const resolved = await page.eval(pgResolve(CONTACT));
    if (resolved.signedOut) throw new SignedOut("Teams message cache absent (signed out)");
    if (resolved.notFound) { log(`contact "${CONTACT}" not found`); process.exit(1); }
    const cid = resolved.conversationId;
    const mriNames = resolved.mriNames || {};
    const { messages } = await page.eval(pgMessages(cid, resolved.contactMri));
    const picked = (messages || []).filter((m) => m.timeMs >= sinceMs);
    const out = [];
    out.push(`# Teams chat with ${resolved.contactName}`);
    out.push("");
    out.push(`Conversation: \`${cid}\``);
    out.push(`Since: ${new Date(sinceMs).toISOString()} · ${picked.length} messages`);
    out.push("");
    let lastDay = "";
    for (const m of picked) {
      const d = new Date(m.timeMs);
      const day = d.toLocaleDateString("en-CA");
      if (day !== lastDay) { out.push(`\n## ${day}\n`); lastDay = day; }
      const { imgMap, paths } = await fetchAttachmentsForMessage(page, m, dir);
      const reply = extractReply(m.content, mriNames);
      const text = htmlToText(m.content, imgMap);
      const name = m.mine ? "Me" : (m.fromName || "Them");
      if (reply) out.push(`**${hhmm(m.timeMs)} ${name}** ${reply}`);
      out.push(`**${hhmm(m.timeMs)} ${name}:** ${text}`);
      for (const p of paths) out.push(`  - attachment: \`${p}\``);
      out.push("");
    }
    process.stdout.write(out.join("\n") + "\n");
  });
}

// ---------------------------------- selftest ---------------------------------

function selftest() {
  let pass = 0, fail = 0;
  const eq = (name, got, want) => {
    const ok = JSON.stringify(got) === JSON.stringify(want);
    if (ok) { pass++; } else { fail++; process.stderr.write(`FAIL ${name}\n  got:  ${JSON.stringify(got)}\n  want: ${JSON.stringify(want)}\n`); }
  };

  // HTML stripping
  eq("plain", htmlToText("hello world"), "hello world");
  eq("entities", htmlToText("Jen &amp; Bob &lt;3 &#39;oil&#39;"), "Jen & Bob <3 'oil'");
  eq("br+div", htmlToText("<div>It got mixed with diesel ?<br />\r\nYeah she ordered oil</div>"), "It got mixed with diesel ?\nYeah she ordered oil");
  eq("anchor", htmlToText('see <a href="https://x.io/p">the page</a> now'), "see the page (https://x.io/p) now");
  eq("anchor-bare", htmlToText('<a href="https://x.io">https://x.io</a>'), "https://x.io");
  eq("img-mapped", htmlToText('<p><img itemid="0-eus-d2-abc" src="https://us-api.asm.skype.com/v1/objects/0-eus-d2-abc/views/imgo" /></p>', { "0-eus-d2-abc": "/s/att/1/0-eus-d2-abc.jpg" }), "[image: /s/att/1/0-eus-d2-abc.jpg]");
  eq("img-unmapped", htmlToText('<p><img itemid="z" src="x" /></p>'), "[image]");
  eq("drop-quote", htmlToText('<blockquote itemtype="http://schema.skype.com/Reply"><p itemprop="preview">old</p></blockquote>\n<p>This is fixed</p>'), "This is fixed");

  // reply extraction
  eq("reply", extractReply('<blockquote itemscope itemtype="http://schema.skype.com/Reply" itemid="123"><strong itemprop="mri" itemid="8:live:huntwt">Display Name</strong><span itemprop="time" itemid="123"></span><p itemprop="preview">the estimate</p></blockquote>\n<p>ok</p>', { "8:live:huntwt": "William Hunt" }), "> [reply to William Hunt: the estimate]");
  eq("no-reply", extractReply("<p>hi</p>", {}), "");

  // burst grouping
  const mk = (t, mine) => ({ timeMs: t, mine });
  const min = 60000;
  const g1 = groupBursts([mk(0), mk(2 * min), mk(30 * min), mk(31 * min)], BURST_GAP_MS);
  eq("bursts-count", g1.length, 2);
  eq("bursts-shape", g1.map((b) => b.length), [2, 2]);
  eq("bursts-empty", groupBursts([], BURST_GAP_MS).length, 0);

  // watermark comparison
  eq("newer-yes", isNewer(Date.parse("2026-09-28T12:00:00Z"), "2026-09-28T11:00:00Z"), true);
  eq("newer-no", isNewer(Date.parse("2026-09-28T10:00:00Z"), "2026-09-28T11:00:00Z"), false);
  eq("newer-eq", isNewer(Date.parse("2026-09-28T11:00:00Z"), "2026-09-28T11:00:00Z"), false);
  eq("newer-empty", isNewer(Date.now(), ""), false);

  // filename / extension derivation
  eq("fn-obj-jpeg", deriveFilename("https://us-api.asm.skype.com/v1/objects/0-eus-d2-abc/views/imgo", "image/jpeg", "0-eus-d2-abc"), "0-eus-d2-abc.jpg");
  eq("fn-obj-png", deriveFilename("https://us-api.asm.skype.com/v1/objects/0-eus-d7-xy/views/imgo", "image/png; charset=x", null), "0-eus-d7-xy.png");
  eq("fn-named", deriveFilename("https://x.sharepoint.com/sites/a/estimate.pdf", "application/pdf", null), "estimate.pdf");
  eq("fn-octet", deriveFilename("https://x/objects/blah/views/imgo", "application/octet-stream", "blah"), "blah.bin");

  // image url extraction
  eq("img-extract", imageUrlsFromHtml('<img itemid="0-a-b" src="https://us-api.asm.skype.com/v1/objects/0-a-b/views/imgo" />').map(x => x.id), ["0-a-b"]);

  process.stderr.write(`\nteams-cdp selftest: ${pass} passed, ${fail} failed\n`);
  process.exit(fail ? 1 : 0);
}

// ----------------------------------- entry -----------------------------------

async function main() {
  const args = process.argv.slice(2);
  if (args.includes("--selftest")) return selftest();
  try {
    if (args.includes("--dump")) await runDump(args);
    else await runPoll();
    process.exit(0);
  } catch (e) {
    if (e instanceof PortClosed) { log(e.message); process.exit(3); }
    if (e instanceof SignedOut) { log(e.message); process.exit(2); }
    log("error: " + (e && e.stack ? e.stack.split("\n").slice(0, 3).join(" | ") : e));
    process.exit(1);
  }
}

main();
