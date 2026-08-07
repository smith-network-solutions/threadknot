import { nativeBootstrap } from "./native";

const TOKEN_KEY = "threadknot.token";
/** Pre-rename key. Read once and carried over, so a phone or browser that
 *  already had the LAN URL open does not get logged out by the rebrand. */
const LEGACY_TOKEN_KEY = "armada.token";

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
  /** Access token, appended as `?token=` to same-origin HTTP requests. */
  token: string;
  isTauri: boolean;
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
      token: info.token,
      isTauri: true,
    };
  }

  // Mobile shell: the device credential is injected before load and stays in
  // memory — never written to web storage, never carried on the URL.
  const native = nativeBootstrap();
  if (native) {
    const proto = window.location.protocol === "https:" ? "wss" : "ws";
    return {
      wsUrl: `${proto}://${window.location.host}/ws?token=${encodeURIComponent(native.token)}`,
      httpBase: window.location.origin,
      token: native.token,
      isTauri: false,
    };
  }

  const params = new URLSearchParams(window.location.search);
  let token = params.get("token");
  if (token) {
    localStorage.setItem(TOKEN_KEY, token);
    params.delete("token");
    const qs = params.toString();
    const clean =
      window.location.pathname + (qs ? `?${qs}` : "") + window.location.hash;
    window.history.replaceState(null, "", clean);
  } else {
    token = readStoredToken();
  }

  const proto = window.location.protocol === "https:" ? "wss" : "ws";
  return {
    wsUrl: `${proto}://${window.location.host}/ws?token=${encodeURIComponent(token)}`,
    httpBase: window.location.origin,
    token,
    isTauri: false,
  };
}

/** Build a token-gated URL for a stored attachment's bytes. `machineId`
 *  streams them from the owning peer machine through this server. */
export function attachmentUrl(
  http: { base: string; token: string },
  threadId: string,
  id: string,
  opts?: { machineId?: string },
): string {
  const q = new URLSearchParams({ thread: threadId, id, token: http.token });
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
  const q = new URLSearchParams({ project: projectId, path, token: http.token });
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
  const q = new URLSearchParams({ id, token: http.token });
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
    token: http.token,
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
  const q = new URLSearchParams({ token: http.token, session: sessionId });
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
