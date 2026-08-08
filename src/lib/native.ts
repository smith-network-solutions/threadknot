// Bridge to the Threadknot mobile shell (React Native WebView). The native app
// injects `window.__THREADKNOT_NATIVE__` before the page loads (credential +
// server identity, so nothing secret rides the URL) and drives navigation by
// calling `window.__threadknotNative.dispatch(...)`. We talk back over
// `window.ReactNativeWebView.postMessage`. In ordinary browsers and the Tauri
// shell every function here is an inert no-op.

export interface NativeBootstrap {
  /** Device credential presented as the ws/HTTP token. Held in memory only.
   *
   *  **Empty on a relay-connected server**, and that is the normal case there:
   *  the strict ingress refuses any credential-bearing query key with a 400 even
   *  when the value is valid, so a token here would break every request rather
   *  than merely being redundant. Authority comes from the `HttpOnly` session
   *  cookie the shell established instead. */
  token?: string;
  /** Double-submit token derived from that cookie. Present only in the cookie
   *  case; `token` and `csrf` are the two alternatives, never both. */
  csrf?: string;
  serverId?: string;
  platform?: "ios" | "android";
  /** Optional shell features. Absent in older mobile builds. */
  capabilities?: string[];
}

export type NativeOutMessage =
  | { type: "ready"; serverId?: string; serverName?: string }
  | { type: "routeChanged"; projectId?: string; threadId?: string }
  | { type: "connectionChanged"; conn: "connecting" | "online" | "offline" }
  | { type: "clipboardRead"; requestId: string }
  | { type: "reloadRequest" };

export interface NativeNavigate {
  type: "navigate";
  projectId?: string;
  threadId: string;
}

interface NativeResume {
  type: "resume";
}

interface NativeClipboardResult {
  type: "clipboardResult";
  requestId: string;
  text?: string;
  error?: string;
}

interface RNWebView {
  postMessage(data: string): void;
}

declare global {
  interface Window {
    __THREADKNOT_NATIVE__?: NativeBootstrap;
    ReactNativeWebView?: RNWebView;
    __threadknotNative?: { dispatch(msg: unknown): void };
  }
}

export function nativeBootstrap(): NativeBootstrap | null {
  // Deliberately NOT gated on a token. It used to be, which silently disabled
  // the entire native bridge for a relay-connected server: no `ready`, no
  // route/connection reporting, no push-tap navigation, no native clipboard —
  // and the app reporting itself offline. On the strict ingress there is no
  // token to have, so requiring one made the remote case unreachable.
  //
  // The shell is still identified by `ReactNativeWebView` (see `isNativeShell`),
  // so an ordinary browser cannot fake its way in here by defining the global.
  return window.__THREADKNOT_NATIVE__ ?? null;
}

export function isNativeShell(): boolean {
  return nativeBootstrap() != null && typeof window.ReactNativeWebView?.postMessage === "function";
}

/** Shells advertise what they can do; older installed builds advertise less. */
export function hasNativeCapability(name: string): boolean {
  return nativeBootstrap()?.capabilities?.includes(name) === true;
}

export function hasNativeClipboard(): boolean {
  return hasNativeCapability("clipboard-read");
}

/**
 * Ask the shell to reload this server's WebView — a real navigation reload, so
 * the bundle is refetched (`index.html` is served `no-cache`) rather than the
 * SPA merely re-mounting. Returns false when there is no shell to ask, in
 * which case the caller should fall back to `location.reload()`.
 */
export function requestNativeReload(): boolean {
  if (!isNativeShell() || !hasNativeCapability("reload")) return false;
  postToNative({ type: "reloadRequest" });
  return true;
}

export function postToNative(msg: NativeOutMessage): void {
  try {
    window.ReactNativeWebView?.postMessage(JSON.stringify(msg));
  } catch {
    // shell gone mid-teardown — fine
  }
}

type NavigateHandler = (nav: NativeNavigate) => void;
type ResumeHandler = () => void;

let navigateHandler: NavigateHandler | null = null;
/** Navigations that arrived before React registered its handler (cold start). */
const queued: NativeNavigate[] = [];
let resumeHandler: ResumeHandler | null = null;
let resumeQueued = false;
let clipboardRequestSeq = 0;
const pendingClipboard = new Map<
  string,
  { resolve: (text: string | null) => void; timer: number }
>();

function isNavigate(msg: unknown): msg is NativeNavigate {
  if (typeof msg !== "object" || msg === null) return false;
  const m = msg as Record<string, unknown>;
  return m.type === "navigate" && typeof m.threadId === "string" && m.threadId.length > 0;
}

function isClipboardResult(msg: unknown): msg is NativeClipboardResult {
  if (typeof msg !== "object" || msg === null) return false;
  const m = msg as Record<string, unknown>;
  return (
    m.type === "clipboardResult" &&
    typeof m.requestId === "string" &&
    (m.text === undefined || typeof m.text === "string") &&
    (m.error === undefined || typeof m.error === "string")
  );
}

function isResume(msg: unknown): msg is NativeResume {
  return (
    typeof msg === "object" &&
    msg !== null &&
    (msg as { type?: unknown }).type === "resume"
  );
}

/**
 * Install the dispatch entry point the native shell calls via
 * `injectJavaScript`. Safe to call in any environment; only validated shell
 * messages ever reach the app.
 */
export function initNativeBridge(): void {
  if (window.__threadknotNative) return;
  window.__threadknotNative = {
    dispatch(msg: unknown) {
      let parsed = msg;
      if (typeof msg === "string") {
        try {
          parsed = JSON.parse(msg);
        } catch {
          return;
        }
      }
      if (isNavigate(parsed)) {
        if (navigateHandler) navigateHandler(parsed);
        else queued.push(parsed);
        return;
      }
      if (isResume(parsed)) {
        if (resumeHandler) resumeHandler();
        else resumeQueued = true;
        return;
      }
      if (isClipboardResult(parsed)) {
        const pending = pendingClipboard.get(parsed.requestId);
        if (!pending) return;
        window.clearTimeout(pending.timer);
        pendingClipboard.delete(parsed.requestId);
        pending.resolve(parsed.error ? null : (parsed.text ?? ""));
      }
    },
  };
}

export function setNativeNavigationHandler(fn: NavigateHandler | null): void {
  navigateHandler = fn;
  if (fn) {
    while (queued.length > 0) {
      const nav = queued.shift();
      if (nav) fn(nav);
    }
  }
}

export function setNativeResumeHandler(fn: ResumeHandler | null): void {
  resumeHandler = fn;
  if (fn && resumeQueued) {
    resumeQueued = false;
    fn();
  }
}

/**
 * Read plain text through the Expo shell's native clipboard module. This
 * bypasses the secure-context restriction that blocks navigator.clipboard on
 * Threadknot's ordinary HTTP LAN URLs.
 */
export function readNativeClipboardText(): Promise<string | null> {
  if (!isNativeShell() || !hasNativeClipboard()) return Promise.resolve(null);
  const requestId = `clip-${Date.now()}-${++clipboardRequestSeq}`;
  return new Promise((resolve) => {
    const timer = window.setTimeout(() => {
      pendingClipboard.delete(requestId);
      resolve(null);
    }, 3000);
    pendingClipboard.set(requestId, { resolve, timer });
    postToNative({ type: "clipboardRead", requestId });
  });
}
