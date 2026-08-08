import { nativeBootstrap } from "./native";

const TOKEN_KEY = "threadknot.token";
/** Pre-rename key. Read once and carried over, so a phone or browser that
 *  already had the LAN URL open does not get logged out by the rebrand. */
const LEGACY_TOKEN_KEY = "armada.token";
/** Per-tab only: `sessionStorage` dies with the tab, and the CSRF token is
 *  worthless without the `HttpOnly` cookie it is derived from. */
const CSRF_KEY = "threadknot.csrf";

/** Double-submit header the strict ingress requires on cookie-authenticated
 *  state changes. Mirrors `ingress::CSRF_HEADER` — the two must agree, and the
 *  failure when they do not is a 403 with no other symptom. */
export const CSRF_HEADER = "x-threadknot-csrf";

function readStoredToken(): string {
  const current = localStorage.getItem(TOKEN_KEY);
  if (current) return current;
  const legacy = localStorage.getItem(LEGACY_TOKEN_KEY);
  if (legacy) {
    localStorage.setItem(TOKEN_KEY, legacy);
    localStorage.removeItem(LEGACY_TOKEN_KEY);
    return legacy;
  }
  return "";
}

export interface ServerTarget {
  wsUrl: string;
  /** Base for HTTP requests (e.g. attachment thumbnails), no trailing slash. */
  httpBase: string;
  /** Access token, appended as `?token=` to same-origin HTTP requests.
   *
   *  **Empty in remote mode**, where authentication is an `HttpOnly` cookie the
   *  browser attaches itself. The strict ingress refuses a credential in a URL
   *  outright, so every builder below omits the parameter rather than sending a
   *  blank one. */
  token: string;
  /** Double-submit token for cookie-authenticated state changes. Empty unless
   *  this is a cookie session. Held in memory only — writing it to storage
   *  would hand it to the same XSS the cookie's `HttpOnly` flag defends
   *  against. */
  csrf: string;
  isTauri: boolean;
  /** True when this origin needs a credential we do not have.
   *
   *  Without this the app used to boot with nothing, open a socket that could
   *  only be refused, and sit on "offline — retrying…" for ever — the first thing
   *  anyone saw after opening their own relay hostname in a browser, with no hint
   *  that pairing was the missing step. The caller renders a pairing screen
   *  instead of connecting. */
  needsPairing: boolean;
}

interface ServerInfo {
  port: number;
  token: string;
  lanUrl: string;
}

function isTauriEnv(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * Figure out where the Threadknot server lives.
 * - Inside the Tauri shell: ask the backend via the `server_info` command.
 * - In a plain browser (phone over LAN): same host that served the page,
 *   token from `?token=` (persisted to localStorage, then stripped from URL).
 *
 * Tauri modules are only ever loaded via dynamic import inside the Tauri
 * branch so the web build runs in ordinary browsers.
 */
export async function discoverServer(): Promise<ServerTarget> {
  if (isTauriEnv()) {
    const { invoke } = await import("@tauri-apps/api/core");
    const info = await invoke<ServerInfo>("server_info");
    return {
      wsUrl: `ws://127.0.0.1:${info.port}/ws?token=${encodeURIComponent(info.token)}`,
      httpBase: `http://127.0.0.1:${info.port}`,
      // The desktop shell holds the master token by construction: it asked the
      // Rust side for it. There is nothing to pair.
      needsPairing: false,
      token: info.token,
      csrf: "",
      isTauri: true,
    };
  }

  // Mobile shell. Two shapes, and which one arrives depends on the ingress the
  // shell reached this server through:
  //
  //  * LAN — a device credential injected before load, kept in memory, never
  //    written to web storage and never put in a URL by anything but the query
  //    parameter the compatibility listener still accepts.
  //  * Relay — no credential at all. The shell established an `HttpOnly` cookie
  //    via `POST /api/session` and passes only the double-submit token. Adding
  //    `?token=` here would be a 400 from the strict ingress on every request,
  //    including `<img src>` attachments that no shell-side interception can
  //    reach.
  const native = nativeBootstrap();
  if (native) {
    const proto = window.location.protocol === "https:" ? "wss" : "ws";
    const token = native.token ?? "";
    return {
      wsUrl: token
        ? `${proto}://${window.location.host}/ws?token=${encodeURIComponent(token)}`
        : `${proto}://${window.location.host}/ws`,
      httpBase: window.location.origin,
      token,
      csrf: token ? "" : (native.csrf ?? ""),
      isTauri: false,
      // A native shell established its own session before the page loaded; if
      // that failed there is a keychain credential to retry with, and no code for
      // a person to type into a WebView.
      needsPairing: false,
    };
  }

  const params = new URLSearchParams(window.location.search);
  const proto = window.location.protocol === "https:" ? "wss" : "ws";

  let token = params.get("token");
  if (token) {
    localStorage.setItem(TOKEN_KEY, token);
    stripFromUrl(params, "token");
  } else {
    token = readStoredToken();
  }

  if (token) {
    return {
      wsUrl: `${proto}://${window.location.host}/ws?token=${encodeURIComponent(token)}`,
      httpBase: window.location.origin,
      token,
      csrf: "",
      isTauri: false,
      needsPairing: false,
    };
  }

  // No token anywhere: either a remote browser that already holds a session
  // cookie, or one arriving with a one-time pairing code to exchange for one.
  // A cookie is what remote access uses instead of a URL credential — the page
  // never sees it, so it cannot leak it.
  const session = await establishSession(params);
  return {
    wsUrl: `${proto}://${window.location.host}/ws`,
    httpBase: window.location.origin,
    token: "",
    csrf: session.csrf,
    isTauri: false,
    needsPairing: !session.paired,
  };
}

/** Redeem a pairing code shown by the desktop, in exchange for a cookie session.
 *
 *  Separate from `establishSession` because this one is driven by a person typing
 *  a code and its failures have to be *shown*, not swallowed: "expired or already
 *  used" is the difference between trying again and showing a fresh code on the
 *  desktop. The server deliberately gives one message for wrong, expired and
 *  already-redeemed, so this passes it through rather than guessing between them.
 */
export async function pairBrowser(code: string): Promise<string> {
  const cleaned = code.replace(/[\s-]/g, "").toUpperCase();
  if (!cleaned) throw new Error("Enter the code shown on the desktop.");
  let resp: Response;
  try {
    resp = await fetch(`${window.location.origin}/api/session`, {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        pairingCode: cleaned,
        platform: "browser",
        deviceName: "Remote browser",
      }),
    });
  } catch {
    throw new Error("Could not reach this machine. Check it is online and try again.");
  }
  if (!resp.ok) {
    const detail = (await resp.text()).trim();
    throw new Error(detail || `Pairing failed (${resp.status}).`);
  }
  const body = (await resp.json()) as { csrf?: string };
  if (body.csrf) sessionStorage.setItem(CSRF_KEY, body.csrf);
  return body.csrf ?? "";
}

/** Drop a parameter from the visible URL without a reload. */
function stripFromUrl(params: URLSearchParams, key: string): void {
  params.delete(key);
  const qs = params.toString();
  window.history.replaceState(
    null,
    "",
    window.location.pathname + (qs ? `?${qs}` : "") + window.location.hash,
  );
}

/** The CSRF half of a cookie session, or "" if there is no usable session.
 *
 *  A pairing code in the URL is redeemed exactly once and stripped immediately:
 *  it is single-use and short-lived, but leaving it in the address bar is how
 *  it ends up in a screenshot or a shared link. */
async function establishSession(
  params: URLSearchParams,
): Promise<{ csrf: string; paired: boolean }> {
  const code = params.get("c") ?? params.get("pair");
  if (code) {
    stripFromUrl(params, params.get("c") ? "c" : "pair");
    try {
      const resp = await fetch(`${window.location.origin}/api/session`, {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ pairingCode: code, platform: "browser" }),
      });
      if (resp.ok) {
        const body = (await resp.json()) as { csrf?: string };
        if (body.csrf) sessionStorage.setItem(CSRF_KEY, body.csrf);
        return { csrf: body.csrf ?? "", paired: true };
      }
    } catch {
      // Fall through to the probe: an already-valid cookie still works.
    }
  }
  // Already holding a cookie? The server answers this one unauthenticated-safe
  // endpoint, so a 200 means the jar still has a live session.
  try {
    const probe = await fetch(`${window.location.origin}/api/server-info`, {
      credentials: "include",
    });
    if (probe.ok) return { csrf: sessionStorage.getItem(CSRF_KEY) ?? "", paired: true };
    // A 401 is the *useful* answer: this origin works, it simply does not know
    // this browser yet. That is a pairing prompt, not an outage.
    if (probe.status === 401 || probe.status === 403) return { csrf: "", paired: false };
  } catch {
    // Unreachable rather than unauthenticated. Let the socket report it, because
    // showing a pairing form for a machine that is switched off would send
    // someone hunting for a code that cannot help.
    return { csrf: "", paired: true };
  }
  return { csrf: "", paired: true };
}

/** The `token=` parameter, or nothing at all in remote mode.
 *
 *  Sending `token=` empty would be worse than sending nothing: the strict
 *  ingress refuses any request whose URL carries a credential key, so a blank
 *  one turns every image and download into a 400. */
function credential(http: { token: string }): Record<string, string> {
  return http.token ? { token: http.token } : {};
}

/** Build a token-gated URL for a stored attachment's bytes. `machineId`
 *  streams them from the owning peer machine through this server. */
export function attachmentUrl(
  http: { base: string; token: string },
  threadId: string,
  id: string,
  opts?: { machineId?: string },
): string {
  const q = new URLSearchParams({ thread: threadId, id, ...credential(http) });
  if (opts?.machineId) q.set("machineId", opts.machineId);
  return `${http.base}/attachment?${q.toString()}`;
}

/** Build a token-gated URL for raw project-file bytes (images, PDF, download). */
export function fileUrl(
  http: { base: string; token: string },
  projectId: string,
  path: string,
  opts?: { download?: boolean; machineId?: string },
): string {
  const q = new URLSearchParams({ project: projectId, path, ...credential(http) });
  if (opts?.download) q.set("download", "1");
  if (opts?.machineId) q.set("machineId", opts.machineId);
  return `${http.base}/file?${q.toString()}`;
}

/** Build a token-gated URL for a produced artifact's durable snapshot bytes. */
export function artifactFileUrl(
  http: { base: string; token: string },
  id: string,
  opts?: { download?: boolean; machineId?: string },
): string {
  const q = new URLSearchParams({ id, ...credential(http) });
  if (opts?.download) q.set("download", "1");
  if (opts?.machineId) q.set("machineId", opts.machineId);
  return `${http.base}/artifact-file?${q.toString()}`;
}

/** WebSocket URL for a pty terminal session (binary = output, text = control). */
export function termWsUrl(
  http: { base: string; token: string },
  projectId: string,
  termId: string,
  size: { cols: number; rows: number },
  machineId?: string,
): string {
  const q = new URLSearchParams({
    ...credential(http),
    project: projectId,
    term: termId,
    cols: String(size.cols),
    rows: String(size.rows),
  });
  // Remote machine: this server splices the socket onto the owner's pty.
  if (machineId) q.set("machineId", machineId);
  return `${http.base.replace(/^http/, "ws")}/term?${q.toString()}`;
}

/** WebSocket URL for a driven browser session (binary = JPEG frames, text = control). */
export function browserWsUrl(
  http: { base: string; token: string },
  sessionId: string,
  opts?: { url?: string; machineId?: string },
): string {
  const q = new URLSearchParams({ session: sessionId, ...credential(http) });
  if (opts?.url) q.set("url", opts.url);
  // Remote machine: this server splices the socket onto the owner's Chrome,
  // so the pane shows (and signs in) that machine's browser.
  if (opts?.machineId) q.set("machineId", opts.machineId);
  return `${http.base.replace(/^http/, "ws")}/browser?${q.toString()}`;
}

/** Open the native directory picker (Tauri only). Returns null if cancelled. */
export async function pickDirectoryNative(): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    directory: true,
    multiple: false,
    title: "Add project folder",
  });
  return typeof picked === "string" ? picked : null;
}
