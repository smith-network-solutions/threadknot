import { nativeBootstrap } from "./native";
import { CSRF_HEADER } from "./discovery";

// Notification plumbing (Traycer-style): native/system notification when the
// window is unfocused, in-app toast + chime otherwise. The LAN phone URL is
// plain http (insecure context), so the Web Notification API often does not
// exist there — the toast + chime path is the universal fallback.

/** How `workspaces` below is read: as a mute list, or as an allowlist. */
export type NotifyScope = "all" | "selected" | "none";

export interface NotifyPrefs {
  enabled: boolean;
  sound: boolean;
  scope: NotifyScope;
  /** Workspace ids — muted when scope is "all", subscribed when "selected".
   *  One list read two ways, so the UI only ever says "notify me: yes/no". */
  workspaces: string[];
}

export interface NativeNotificationReceipt {
  platform: string;
  identity: string;
  notificationId?: number;
  connectionReused: boolean;
}

const LS_NOTIFY_OFF = "threadknot.notifyOff";
const LS_SOUND_OFF = "threadknot.soundOff";
const LS_SCOPE = "threadknot.notifyScope";
const LS_WORKSPACES = "threadknot.notifyWorkspaces";

// Prefs live per browser profile / per WebView, which is exactly the identity
// we want: two people on one Threadknot are two clients. React reads them through
// `useSyncExternalStore`, so the snapshot has to be referentially stable —
// hence the cache, invalidated on every write (including writes from another
// tab, via the `storage` event).
let cached: NotifyPrefs | null = null;
const listeners = new Set<() => void>();

function readScope(): NotifyScope {
  const raw = localStorage.getItem(LS_SCOPE);
  return raw === "selected" || raw === "none" ? raw : "all";
}

function readWorkspaces(): string[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(LS_WORKSPACES) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((v): v is string => typeof v === "string") : [];
  } catch {
    return [];
  }
}

export function getNotifyPrefs(): NotifyPrefs {
  cached ??= {
    enabled: localStorage.getItem(LS_NOTIFY_OFF) == null,
    sound: localStorage.getItem(LS_SOUND_OFF) == null,
    scope: readScope(),
    workspaces: readWorkspaces(),
  };
  return cached;
}

export function setNotifyPrefs(p: NotifyPrefs): void {
  if (p.enabled) localStorage.removeItem(LS_NOTIFY_OFF);
  else localStorage.setItem(LS_NOTIFY_OFF, "1");
  if (p.sound) localStorage.removeItem(LS_SOUND_OFF);
  else localStorage.setItem(LS_SOUND_OFF, "1");
  localStorage.setItem(LS_SCOPE, p.scope);
  localStorage.setItem(LS_WORKSPACES, JSON.stringify(p.workspaces));
  cached = p;
  for (const fn of listeners) fn();
  void syncNotifyPrefsToShell(p);
}

/** Subscribe to pref changes (this tab or another one). */
export function subscribeNotifyPrefs(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

/** Another tab wrote prefs — drop the snapshot so readers re-read. */
export function invalidateNotifyPrefs(): void {
  cached = null;
  for (const fn of listeners) fn();
}

/** Flip one workspace's subscription, in whichever direction the scope means. */
export function toggleWorkspaceNotify(workspaceId: string): NotifyPrefs {
  const prefs = getNotifyPrefs();
  const listed = prefs.workspaces.includes(workspaceId);
  const next: NotifyPrefs = {
    ...prefs,
    workspaces: listed
      ? prefs.workspaces.filter((id) => id !== workspaceId)
      : [...prefs.workspaces, workspaceId],
  };
  setNotifyPrefs(next);
  return next;
}

/**
 * Whether this workspace is subscribed, ignoring the global alerts switch.
 * This is what a per-workspace control must show: with alerts off, the gate
 * below is false for everything, and driving a toggle from it would invert
 * the meaning of the next click under a mute list.
 */
export function isWorkspaceSubscribed(prefs: NotifyPrefs, workspaceId: string): boolean {
  const listed = prefs.workspaces.includes(workspaceId);
  return prefs.scope === "all" ? !listed : prefs.scope === "selected" && listed;
}

/**
 * Whether this client wants to be alerted about `workspaceId`. An unresolved
 * workspace fails closed under an allowlist: better a missed alert on this
 * client than one leaking to someone who deliberately narrowed their list.
 */
export function wantsWorkspace(prefs: NotifyPrefs, workspaceId: string | undefined): boolean {
  if (!prefs.enabled) return false;
  const listed = workspaceId != null && prefs.workspaces.includes(workspaceId);
  switch (prefs.scope) {
    case "all":
      return !listed;
    case "selected":
      return listed;
    case "none":
      return false;
  }
}

/**
 * Inside the phone shell the same choice has to reach the server, because a
 * sleeping phone is woken by Expo and never sees the WebSocket event this
 * client filters. Best-effort: the local prefs are still authoritative for
 * anything rendered in-app.
 */
async function syncNotifyPrefsToShell(p: NotifyPrefs): Promise<void> {
  const native = nativeBootstrap();
  if (!native) return;
  // Two authentication shapes, matching the two ingresses the shell may have
  // reached this server through — see `NativeBootstrap`. On the relay there is no
  // credential to send: the strict ingress refuses a credential in the body just
  // as it refuses one in the URL, and it resolves the cookie *before* it looks at
  // a bearer, so a cookie-authenticated state change needs the double-submit
  // header or it is a 403. Without this, per-workspace notification
  // subscriptions set from the web UI silently never reached a relay-connected
  // machine.
  const headers: Record<string, string> = { "content-type": "application/json" };
  const body: Record<string, unknown> = {
    // Deliberately not `notificationsEnabled`: that master switch belongs to
    // the shell's own settings screen, which re-asserts it from its stored
    // profile on every launch. Scope "none" already silences the device.
    notifyScope: p.enabled ? p.scope : "none",
    notifyWorkspaces: p.workspaces,
  };
  if (native.token) {
    body.credential = native.token;
  } else if (native.csrf) {
    headers[CSRF_HEADER] = native.csrf;
  } else {
    // Neither shape available: the shell has not finished establishing a session.
    // Sending anyway would be a guaranteed 403, and the shell re-asserts the
    // subscription from its stored profile on the next launch regardless.
    return;
  }
  try {
    await fetch(`${window.location.origin}/api/mobile/push`, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
    });
  } catch {
    // offline or shell gone — the phone keeps its last saved subscription
  }
}

/** Whether the browser Notification API exists and could still be enabled. */
export function webNotifyState(): "unsupported" | "default" | "granted" | "denied" {
  if (typeof Notification === "undefined") return "unsupported";
  return Notification.permission;
}

export async function requestWebPermission(): Promise<boolean> {
  if (typeof Notification === "undefined") return false;
  try {
    return (await Notification.requestPermission()) === "granted";
  } catch {
    return false;
  }
}

/**
 * Show a system-level notification. Returns true if one was shown.
 * Tauri: native desktop notification via the `notify` command.
 * Browser: Web Notification API when available and granted.
 */
export async function showSystemNotification(
  title: string,
  body: string,
  opts: { isTauri: boolean; onClick?: () => void },
): Promise<boolean> {
  if (opts.isTauri) {
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke<NativeNotificationReceipt>("notify", { title, body });
      return true;
    } catch {
      return false;
    }
  }
  if (typeof Notification !== "undefined" && Notification.permission === "granted") {
    try {
      const n = new Notification(title, { body, tag: `threadknot-${title}` });
      n.onclick = () => {
        window.focus();
        opts.onClick?.();
        n.close();
      };
      return true;
    } catch {
      return false;
    }
  }
  return false;
}

/** Send a native test notification even while Threadknot is focused. */
export async function testNativeNotification(): Promise<NativeNotificationReceipt> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<NativeNotificationReceipt>("test_notification");
}

/** Soft two-tone chime (WebAudio; silently no-ops if blocked/unavailable). */
export function chime(): void {
  try {
    const Ctx = window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!Ctx) return;
    const ctx = new Ctx();
    const gain = ctx.createGain();
    gain.gain.value = 0.0001;
    gain.connect(ctx.destination);
    const now = ctx.currentTime;
    for (const [freq, at] of [
      [880, 0],
      [1174.7, 0.12],
    ] as const) {
      const osc = ctx.createOscillator();
      osc.type = "sine";
      osc.frequency.value = freq;
      osc.connect(gain);
      osc.start(now + at);
      osc.stop(now + at + 0.22);
    }
    gain.gain.exponentialRampToValueAtTime(0.06, now + 0.02);
    gain.gain.exponentialRampToValueAtTime(0.0001, now + 0.4);
    setTimeout(() => void ctx.close().catch(() => undefined), 700);
  } catch {
    // audio unavailable — fine
  }
}

/** Try a short vibration on phones (best-effort; ignored on desktop). */
export function vibrate(): void {
  try {
    navigator.vibrate?.([90, 60, 90]);
  } catch {
    // unsupported — fine
  }
}
