import { useCallback, useEffect, useRef, useState } from "react";
import type { BrowserProfileInfo, PortInfo, Project } from "../lib/protocol";
import { findThread, useStore } from "../state/store";
import { browserWsUrl } from "../lib/discovery";
import { isNativeShell, readNativeClipboardText } from "../lib/native";
import { APPEARANCE_EVENT, getPaneZoom } from "../lib/appearance";
import { BROWSER_INTENT_EVENT, takeBrowserUrl } from "../lib/browserIntent";
import { ContextMenu, type CtxItem } from "./ContextMenu";
import "../styles/browser.css";

/** Device-width presets. `null` = fill the pane (native viewport). */
const DEVICES: { id: string; label: string; width: number | null; height: number | null }[] = [
  { id: "fill", label: "Fill", width: null, height: null },
  { id: "iphone", label: "iPhone", width: 390, height: 844 },
  { id: "pixel", label: "Pixel", width: 412, height: 915 },
  { id: "ipad", label: "iPad", width: 820, height: 1180 },
];

type ConnectionState = "idle" | "connecting" | "live" | "retrying";
type ActivityPhase = "started" | "targeting" | "completed" | "failed";

interface BrowserBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

interface BrowserActivity {
  id: string;
  actor: "agent" | "user";
  phase: ActivityPhase;
  action: string;
  label: string;
  detail?: string;
  x?: number;
  y?: number;
  bounds?: BrowserBounds;
  ts: string;
}

interface BrowserDialog {
  kind: string;
  message: string;
  defaultPrompt?: string;
  url?: string;
}

interface BrowserTab {
  id: string;
  url: string;
  title: string;
  active: boolean;
}

/** What the backend hit-test found under a right-click. Everything is
 *  optional: a click on blank page yields only the always-present flags. */
interface BrowserContext {
  selection?: string;
  linkHref?: string;
  imgSrc?: string;
  editable?: boolean;
  /** Vault info riding along with the probe (memory-only on the backend), so
   *  the menu already knows whether it can offer direct fill entries. Absent
   *  when the machine process predates it. `entries` absent means "not
   *  prefetched yet", which is different from an empty list. */
  bitwarden?: {
    state: VaultState;
    entries?: VaultEntry[];
    keyPresent?: boolean;
    requireKey?: boolean;
  };
}

/** An open right-click menu: where to anchor it (viewport px) plus the page
 *  context that decides which items it offers. */
interface BrowserMenu extends BrowserContext {
  x: number;
  y: number;
}

/** Where the Bitwarden vault stands, as reported by the backend. */
type VaultState = "notInstalled" | "loggedOut" | "locked" | "unlocked";

interface VaultEntry {
  id: string;
  name: string;
  username: string;
}

/** The Bitwarden sheet: unlocking, or choosing which login to fill. Null when
 *  nothing is open. */
interface VaultSheet {
  state: VaultState;
  items: VaultEntry[];
  error: string | null;
  busy: boolean;
  /** Physical-gate facts for the sheet's toggle row. */
  keyPresent: boolean;
  requireKey: boolean;
}

const EMPTY_SHEET: VaultSheet = {
  state: "locked",
  items: [],
  error: null,
  busy: false,
  keyPresent: false,
  requireKey: false,
};

/** Prefix a bare host/path with http:// so navigation gets a real URL. */
function normalizeUrl(raw: string): string {
  const value = raw.trim();
  if (!value) return "";
  if (/^[a-z][a-z\d+.-]*:/i.test(value)) return value;
  return `http://${value}`;
}

function isActivity(value: unknown): value is BrowserActivity {
  if (!value || typeof value !== "object") return false;
  const item = value as Partial<BrowserActivity>;
  return typeof item.id === "string" && typeof item.action === "string" && typeof item.label === "string";
}

function modifierMask(event: React.KeyboardEvent): number {
  return (event.altKey ? 1 : 0) + (event.ctrlKey ? 2 : 0) + (event.metaKey ? 4 : 0) + (event.shiftKey ? 8 : 0);
}

/** Paste belongs to the outer app, not the isolated Chrome process. Letting
 * the browser perform this shortcut produces an `onPaste` event whose payload
 * we can explicitly forward to Chrome. */
function isPasteShortcut(event: React.KeyboardEvent): boolean {
  return (
    (!event.altKey && (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "v") ||
    (!event.ctrlKey && !event.metaKey && event.shiftKey && event.key === "Insert")
  );
}

/** How long the agent's target box / cursor stays on screen after an action.
 *  Long enough to see what was touched, short enough that it never reads as a
 *  marker on whatever the page became. */
const MARKER_TTL_MS = 2200;

function activityTime(ts: string): string {
  const date = new Date(ts);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function tabLabel(tab: BrowserTab): string {
  if (tab.title.trim()) return tab.title;
  try {
    return new URL(tab.url).hostname || tab.url || "New tab";
  } catch {
    return tab.url || "New tab";
  }
}

/**
 * Shared browser cockpit. A real isolated Chrome session lives in the Rust
 * backend; both the agent's MCP tools and this human control surface drive that
 * exact session. Closing or changing workspace views only detaches this view —
 * it never destroys the browser.
 */
export function BrowserPane({
  project,
  active,
  sessionId,
  machineId,
}: {
  project: Project;
  active: boolean;
  sessionId: string;
  /** Owning machine, when this workspace lives on a peer. Its Chrome and its
   *  stored logins are the ones this pane drives. */
  machineId?: string;
}) {
  const { state, actions } = useStore();
  const http = state.http;

  const paneRef = useRef<HTMLDivElement | null>(null);
  const stageRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const moreRef = useRef<HTMLDivElement | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const frameSize = useRef({ w: 0, h: 0 });
  const editingRef = useRef(false);
  const pointerFrame = useRef<number | null>(null);
  const pendingPointer = useRef<{ x: number; y: number } | null>(null);
  const pressedButton = useRef<string | null>(null);
  /** Resolvers for in-flight right-click hit-tests, keyed by the nonce sent to
   *  the backend. The `context` frame that comes back settles the promise so
   *  the menu opens with what's actually under the cursor. */
  const probeWaiters = useRef(new Map<string, (ctx: BrowserContext) => void>());
  const probeSeq = useRef(0);

  const [enabled, setEnabled] = useState(active);
  const [url, setUrl] = useState("");
  const [addr, setAddr] = useState("");
  const [editing, setEditing] = useState(false);
  const [device, setDevice] = useState("fill");
  const [connection, setConnection] = useState<ConnectionState>("idle");
  const [supportsPasteControl, setSupportsPasteControl] = useState(false);
  const [activities, setActivities] = useState<BrowserActivity[]>([]);
  const [tabs, setTabs] = useState<BrowserTab[]>([]);
  const [activityOpen, setActivityOpen] = useState(false);
  /** Whether the agent's target box / cursor is still worth drawing. */
  const [markerLive, setMarkerLive] = useState(false);
  const [profiles, setProfiles] = useState<BrowserProfileInfo[]>([]);
  const [profilesLoaded, setProfilesLoaded] = useState(false);
  const [dialog, setDialog] = useState<BrowserDialog | null>(null);
  const [dialogReply, setDialogReply] = useState("");
  const [pasteOpen, setPasteOpen] = useState(false);
  const [pasteText, setPasteText] = useState("");
  const [readingClipboard, setReadingClipboard] = useState(false);
  const [layoutRevision, setLayoutRevision] = useState(0);
  const [stageRevision, setStageRevision] = useState(0);
  const [fullscreen, setFullscreen] = useState(false);
  const [moreOpen, setMoreOpen] = useState(false);

  const [ports, setPorts] = useState<PortInfo[]>([]);
  const [scanning, setScanning] = useState(false);
  const [scanned, setScanned] = useState(false);
  const [menu, setMenu] = useState<BrowserMenu | null>(null);
  const [vault, setVault] = useState<VaultSheet | null>(null);
  const [vaultPassword, setVaultPassword] = useState("");
  /** Armed when the user submits an unlock: the reply may auto-fill a single
   *  matching login. A ref, not state — it must be readable inside the socket
   *  handler without re-subscribing it. */
  const autoFillRef = useRef(false);

  useEffect(() => {
    editingRef.current = editing;
  }, [editing]);

  useEffect(() => {
    if (active) setEnabled(true);
  }, [active]);

  // Keep the affordance honest when the person leaves fullscreen with Esc or
  // the browser's native control rather than the button in our chrome.
  useEffect(() => {
    const syncFullscreen = () => setFullscreen(document.fullscreenElement === paneRef.current);
    syncFullscreen();
    document.addEventListener("fullscreenchange", syncFullscreen);
    return () => document.removeEventListener("fullscreenchange", syncFullscreen);
  }, []);

  useEffect(() => {
    setDialogReply(dialog?.defaultPrompt ?? "");
  }, [dialog]);

  // Secondary browser controls live in one quiet popover. Dismiss it using
  // the same gestures people expect from a native browser menu.
  useEffect(() => {
    if (!moreOpen) return;
    const closeOnOutsidePress = (event: MouseEvent) => {
      if (!moreRef.current?.contains(event.target as Node)) setMoreOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMoreOpen(false);
    };
    document.addEventListener("mousedown", closeOnOutsidePress);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("mousedown", closeOnOutsidePress);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [moreOpen]);

  const rescan = useCallback(async () => {
    setScanning(true);
    try {
      const result = await actions.scanPorts();
      setPorts(result.ports ?? []);
    } catch {
      setPorts([]);
    } finally {
      setScanning(false);
      setScanned(true);
    }
  }, [actions]);

  useEffect(() => {
    if (active && !scanned && !scanning) void rescan();
  }, [active, rescan, scanned, scanning]);

  const send = useCallback((message: unknown) => {
    const socket = wsRef.current;
    if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(message));
  }, []);

  const navigate = useCallback(
    (raw: string) => {
      const next = normalizeUrl(raw);
      if (!next) return;
      setUrl(next);
      setAddr(next);
      setEditing(false);
      send({ type: "navigate", url: next });
    },
    [send],
  );

  // "Open in Threadknot Browser" from the link chooser (LinkOpenModal). The
  // URL sits in a mailbox because the choice may open this pane before it has
  // mounted or its socket is live; claim it at every step that could be the
  // last one (mount, intent event, connection turning live).
  const intentRef = useRef<string | null>(null);
  useEffect(() => {
    const claim = () => {
      const staged = takeBrowserUrl(project.id);
      if (staged) intentRef.current = staged;
      if (intentRef.current && connection === "live") {
        navigate(intentRef.current);
        intentRef.current = null;
      }
    };
    claim();
    window.addEventListener(BROWSER_INTENT_EVENT, claim);
    return () => window.removeEventListener(BROWSER_INTENT_EVENT, claim);
  }, [project.id, navigate, connection]);

  const paint = useCallback((buffer: ArrayBuffer) => {
    const blob = new Blob([buffer], { type: "image/jpeg" });
    createImageBitmap(blob)
      .then((bitmap) => {
        const canvas = canvasRef.current;
        if (!canvas) {
          bitmap.close();
          return;
        }
        frameSize.current = { w: bitmap.width, h: bitmap.height };
        if (canvas.width !== bitmap.width || canvas.height !== bitmap.height) {
          canvas.width = bitmap.width;
          canvas.height = bitmap.height;
        }
        canvas.getContext("2d")?.drawImage(bitmap, 0, 0);
        bitmap.close();
        setLayoutRevision((value) => value + 1);
      })
      .catch(() => {
        /* Drop malformed or superseded frames. */
      });
  }, []);

  // The on-canvas target box and agent cursor are anchored in PAGE coordinates,
  // so they stop describing anything real the moment the document changes — and
  // because they simply track the newest activity, they used to sit on screen
  // until the *next* action arrived (a typed submit left a box hovering over the
  // page it had navigated to). Two rules keep the marker honest: it fades on a
  // short timer, and a navigation clears it at once. Neither touches the
  // activity trail in the side panel, which is meant to persist.
  //
  // Clearing on nav is necessary but NOT sufficient on its own: the server
  // back-fills a follow-up event's missing x/y/bounds from the action's earlier
  // event (see `emit_activity`), so the `completed` for a typed submit lands
  // AFTER the navigation still carrying the old document's coordinates. Taken at
  // face value that re-arms the marker over the page it just navigated to. So
  // the marker is scoped to a document generation: an action is only allowed to
  // paint on the document it actually started on.
  const markerTimer = useRef<number | null>(null);
  /** Bumped on every navigation; identifies the current document. */
  const navSeq = useRef(0);
  /** Activity id -> the document generation that action began on. */
  const activityDoc = useRef(new Map<string, number>());

  /** Generation an action belongs to, remembered from its FIRST event so later
   *  phases of the same action inherit it rather than adopting the new page. */
  const docOf = (id: string): number => {
    const seen = activityDoc.current.get(id);
    if (seen !== undefined) return seen;
    if (activityDoc.current.size > 200) activityDoc.current.clear();
    activityDoc.current.set(id, navSeq.current);
    return navSeq.current;
  };

  const stopMarkerTimer = () => {
    if (markerTimer.current != null) {
      window.clearTimeout(markerTimer.current);
      markerTimer.current = null;
    }
  };

  const showMarker = useCallback(() => {
    stopMarkerTimer();
    setMarkerLive(true);
    markerTimer.current = window.setTimeout(() => {
      markerTimer.current = null;
      setMarkerLive(false);
    }, MARKER_TTL_MS);
  }, []);

  const clearMarker = useCallback(() => {
    stopMarkerTimer();
    setMarkerLive(false);
  }, []);

  useEffect(() => stopMarkerTimer, []);

  const mergeActivity = useCallback((next: BrowserActivity) => {
    // Arm only for a positioned action that belongs to the CURRENT document.
    // A bounds-less action (evaluate, wait_for, screenshot) must not resurrect
    // the previous element's box, and neither may a late phase of an action
    // whose coordinates were measured against a page that is already gone.
    const positioned = next.x != null || next.y != null || next.bounds;
    if (positioned && docOf(next.id) === navSeq.current) showMarker();
    setActivities((previous) => {
      const existing = previous.find((item) => item.id === next.id);
      const merged = existing
        ? {
            ...existing,
            ...next,
            detail: next.detail ?? existing.detail,
            x: next.x ?? existing.x,
            y: next.y ?? existing.y,
            bounds: next.bounds ?? existing.bounds,
          }
        : next;
      return [...previous.filter((item) => item.id !== next.id), merged].slice(-40);
    });
  }, [showMarker]);

  // Connect after first activation. This effect intentionally does not depend
  // on `active`: changing workspace tabs leaves the socket and Chrome session
  // alive. Only changing the thread/server or unmounting detaches the view.
  useEffect(() => {
    if (!enabled || !http) return;
    let disposed = false;
    let socket: WebSocket | null = null;
    let reconnectTimer: number | null = null;
    let retry = 0;

    const scheduleReconnect = () => {
      if (disposed || document.hidden || reconnectTimer != null) return;
      retry += 1;
      setConnection("retrying");
      reconnectTimer = window.setTimeout(() => {
        reconnectTimer = null;
        connect();
      }, Math.min(500 * 2 ** Math.min(retry, 4), 8000));
    };

    const connect = () => {
      if (disposed || document.hidden) return;
      setSupportsPasteControl(false);
      setConnection(retry > 0 ? "retrying" : "connecting");
      const next = new WebSocket(browserWsUrl(http, sessionId, { machineId }));
      next.binaryType = "arraybuffer";
      socket = next;
      wsRef.current = next;

      next.onopen = () => {
        if (disposed || socket !== next) return;
        retry = 0;
        setConnection("live");
      };
      next.onmessage = (event) => {
        if (typeof event.data !== "string") {
          paint(event.data as ArrayBuffer);
          return;
        }
        try {
          const message = JSON.parse(event.data) as Record<string, unknown>;
          if (message.type === "capabilities") {
            setSupportsPasteControl(message.paste === true);
          } else if (message.type === "nav" && typeof message.url === "string") {
            setUrl(message.url);
            if (!editingRef.current) setAddr(message.url);
            // The document just changed under the marker; its page coordinates
            // no longer point at the element the agent touched. Bumping the
            // generation also stops in-flight events for the previous page from
            // re-arming it once they arrive.
            navSeq.current += 1;
            clearMarker();
          } else if (message.type === "activity" && isActivity(message.activity)) {
            mergeActivity(message.activity);
          } else if (message.type === "activityHistory" && Array.isArray(message.activities)) {
            setActivities(message.activities.filter(isActivity).slice(-40));
          } else if (message.type === "dialog") {
            setDialog((message.dialog as BrowserDialog | null) ?? null);
          } else if (message.type === "tabs" && Array.isArray(message.tabs)) {
            setTabs(message.tabs as BrowserTab[]);
          } else if (message.type === "context" && typeof message.nonce === "string") {
            const resolve = probeWaiters.current.get(message.nonce);
            if (resolve) {
              probeWaiters.current.delete(message.nonce);
              const bw = message.bitwarden as BrowserContext["bitwarden"] | undefined;
              resolve({
                selection: typeof message.selection === "string" ? message.selection : undefined,
                linkHref: typeof message.linkHref === "string" ? message.linkHref : undefined,
                imgSrc: typeof message.imgSrc === "string" ? message.imgSrc : undefined,
                editable: message.editable === true,
                bitwarden: bw && typeof bw.state === "string" ? bw : undefined,
              });
            }
          } else if (message.type === "bitwarden") {
            // One frame type carries every vault answer: a state report (with
            // the current origin's prefetched entries), a list, or a fill
            // outcome. Each updates only the part it speaks to.
            const entries = Array.isArray(message.entries)
              ? (message.entries as VaultEntry[])
              : Array.isArray(message.items)
                ? (message.items as VaultEntry[])
                : null;
            const failedFill = message.filled === false && typeof message.error === "string";
            // The one-click flow: an unlock the user just submitted, answered
            // with exactly one login for this site, fills it without another
            // decision — the choice a picker would offer has only one option.
            if (
              autoFillRef.current &&
              message.state === "unlocked" &&
              entries &&
              typeof message.error !== "string"
            ) {
              autoFillRef.current = false;
              if (entries.length === 1) {
                send({ type: "bwFill", id: entries[0].id });
                setVault((prev) => (prev ? { ...prev, state: "unlocked", items: entries, busy: true } : prev));
                return;
              }
            }
            setVault((prev) => {
              if (message.filled === true) return null; // done — close the sheet
              // A failed fill launched straight from the menu has no sheet on
              // screen to carry its message; open one, or the click just
              // silently does nothing.
              const base: VaultSheet = prev ?? EMPTY_SHEET;
              if (!prev && !failedFill) return prev; // background state push, nothing open
              return {
                state: (message.state as VaultState) ?? base.state,
                items: entries ?? base.items,
                error: typeof message.error === "string" ? message.error : null,
                busy: false,
                keyPresent: typeof message.keyPresent === "boolean" ? message.keyPresent : base.keyPresent,
                requireKey: typeof message.requireKey === "boolean" ? message.requireKey : base.requireKey,
              };
            });
            if (message.state === "unlocked" && !entries) send({ type: "bwList" });
            if (message.filled === true) {
              setVaultPassword("");
              canvasRef.current?.focus();
            }
          } else if (message.type === "engine" && message.state === "stopped") {
            setConnection("retrying");
          }
        } catch {
          /* Ignore unknown future control messages. */
        }
      };
      next.onclose = () => {
        if (wsRef.current === next) wsRef.current = null;
        if (disposed || socket !== next) return;
        setConnection("retrying");
        scheduleReconnect();
      };
      next.onerror = () => next.close();
    };

    const onVisibility = () => {
      if (!document.hidden && (!socket || socket.readyState === WebSocket.CLOSED)) {
        connect();
      }
    };

    connect();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      disposed = true;
      document.removeEventListener("visibilitychange", onVisibility);
      if (reconnectTimer != null) window.clearTimeout(reconnectTimer);
      if (wsRef.current === socket) wsRef.current = null;
      socket?.close();
    };
  }, [clearMarker, enabled, http, machineId, mergeActivity, paint, sessionId]);

  useEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;
    const observer = new ResizeObserver(() => setStageRevision((value) => value + 1));
    observer.observe(stage);
    return () => observer.disconnect();
  }, [enabled]);

  useEffect(
    () => () => {
      if (pointerFrame.current != null) cancelAnimationFrame(pointerFrame.current);
    },
    [],
  );

  // Pane zoom (ctrl+wheel, see hotwheel.ts). The container's CSS zoom scales
  // the chrome and the canvas bitmap; in fill mode we also shrink the remote
  // CSS viewport by the same factor so the page genuinely reflows larger
  // instead of just magnifying, and the zoomed canvas stays width-bound with
  // vertical-only scrolling. Device presets keep their exact emulated sizes.
  const [browserZoom, setBrowserZoom] = useState(() => getPaneZoom("browser"));
  useEffect(() => {
    const onAppearance = () => setBrowserZoom(getPaneZoom("browser"));
    window.addEventListener(APPEARANCE_EVENT, onAppearance);
    return () => window.removeEventListener(APPEARANCE_EVENT, onAppearance);
  }, []);

  // Push device-size changes to the backend viewport.
  useEffect(() => {
    if (connection !== "live") return;
    const preset = DEVICES.find((item) => item.id === device) ?? DEVICES[0];
    if (preset.width && preset.height) {
      send({ type: "resize", width: preset.width, height: preset.height });
    } else if (fullscreen && stageRef.current) {
      // In fullscreen, "Fill" should be a real fullscreen viewport instead
      // of a fixed 1280 × 800 frame scaled up by the canvas. The stage's rect
      // is in visual pixels, so undo pane zoom before giving CDP its CSS size.
      const rect = stageRef.current.getBoundingClientRect();
      send({
        type: "resize",
        width: Math.max(1, Math.round(rect.width / browserZoom)),
        height: Math.max(1, Math.round(rect.height / browserZoom)),
      });
    } else {
      send({
        type: "resize",
        width: Math.round(1280 / browserZoom),
        height: Math.round(800 / browserZoom),
      });
    }
  }, [connection, device, send, browserZoom, fullscreen, stageRevision]);

  function toFrameCoords(
    event: React.PointerEvent | React.WheelEvent | React.MouseEvent,
  ): { x: number; y: number } {
    const canvas = canvasRef.current;
    if (!canvas) return { x: 0, y: 0 };
    const rect = canvas.getBoundingClientRect();
    const width = frameSize.current.w || canvas.width || rect.width;
    const height = frameSize.current.h || canvas.height || rect.height;
    return {
      x: Math.round(((event.clientX - rect.left) / rect.width) * width),
      y: Math.round(((event.clientY - rect.top) / rect.height) * height),
    };
  }

  function buttonName(button: number): string {
    return button === 2 ? "right" : button === 1 ? "middle" : "left";
  }

  function queuePointerMove(point: { x: number; y: number }) {
    pendingPointer.current = point;
    if (pointerFrame.current != null) return;
    pointerFrame.current = requestAnimationFrame(() => {
      pointerFrame.current = null;
      if (pendingPointer.current) send({ type: "mouse", event: "moved", ...pendingPointer.current });
    });
  }

  function releasePointer(event: React.PointerEvent) {
    const button = pressedButton.current;
    if (!button) return;
    pressedButton.current = null;
    const point = toFrameCoords(event);
    send({ type: "mouse", event: "released", ...point, button, clickCount: event.detail || 1 });
    (event.currentTarget as HTMLCanvasElement).releasePointerCapture?.(event.pointerId);
    // The right-click menu opens from pointerup, NOT from `contextmenu`: the
    // pointerdown handler above calls preventDefault (it has to, or the host
    // starts a text selection / drag over the canvas), and that suppresses the
    // compatibility mouse events Blink derives `contextmenu` from — so that
    // event never arrives here. This is also the moment the page itself has
    // finished handling the right press, so the probe sees the field the click
    // just focused.
    if (button === "right" && event.type === "pointerup") {
      const { clientX, clientY } = event;
      void probeContext(point).then((ctx) => setMenu({ x: clientX, y: clientY, ...ctx }));
    }
  }

  // Translate frame-native agent coordinates into the currently scaled canvas.
  void layoutRevision;
  const latestActivity = activities[activities.length - 1];
  const canvasRect = canvasRef.current?.getBoundingClientRect();
  const stageRect = stageRef.current?.getBoundingClientRect();
  const nativeWidth = frameSize.current.w || canvasRef.current?.width || 0;
  const nativeHeight = frameSize.current.h || canvasRef.current?.height || 0;
  // Belt and braces alongside the arm-time check: never paint coordinates that
  // were measured against a document this pane has already navigated away from.
  const markerFresh =
    markerLive && latestActivity != null && activityDoc.current.get(latestActivity.id) === navSeq.current;
  const activityPoint =
    markerFresh && latestActivity?.x != null && latestActivity.y != null && canvasRect && stageRect && nativeWidth > 0 && nativeHeight > 0
      ? {
          left: canvasRect.left - stageRect.left + (latestActivity.x / nativeWidth) * canvasRect.width,
          top: canvasRect.top - stageRect.top + (latestActivity.y / nativeHeight) * canvasRect.height,
        }
      : null;
  const activityBounds =
    markerFresh && latestActivity?.bounds && canvasRect && stageRect && nativeWidth > 0 && nativeHeight > 0
      ? {
          left: canvasRect.left - stageRect.left + (latestActivity.bounds.x / nativeWidth) * canvasRect.width,
          top: canvasRect.top - stageRect.top + (latestActivity.bounds.y / nativeHeight) * canvasRect.height,
          width: (latestActivity.bounds.width / nativeWidth) * canvasRect.width,
          height: (latestActivity.bounds.height / nativeHeight) * canvasRect.height,
        }
      : null;

  // Signed-in profiles: which one this chat's browser uses, if any. Attaching
  // one swaps the underlying Chrome for a durable, origin-scoped profile.
  const thread = state.activeThreadId ? findThread(state, state.activeThreadId) : null;
  const attachedId = thread?.settings.browserProfileId ?? "";
  const attached = profiles.find((profile) => profile.id === attachedId) ?? null;
  const profileTitle = attached
    ? attached.origins.includes("*")
      ? `Signed in as ${attached.name} — this browser keeps the login and may visit any site`
      : `Signed in as ${attached.name} — this browser may only visit ${attached.origins.join(", ")}`
    : profilesLoaded && attachedId
      ? "This chat points at a signed-in profile that no longer exists (it was deleted). Switch to Disposable, or attach another profile."
      : "Disposable browser: nothing stays signed in. Pick a profile to keep a login.";

  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    void actions
      .listBrowserProfiles(machineId)
      .then((list) => {
        if (cancelled) return;
        setProfiles(list);
        setProfilesLoaded(true);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [actions, enabled, machineId]);

  const attachProfile = (profileId: string) => {
    if (!thread) return;
    void actions
      .setSettings({ ...thread.settings, browserProfileId: profileId || null })
      .catch(() => undefined);
  };

  const restartBrowser = () => {
    setActivities([]);
    setDialog(null);
    setTabs([]);
    setUrl("");
    setAddr("");
    setConnection("retrying");
    send({ type: "reset" });
  };

  const answerDialog = (accept: boolean) => {
    send({ type: "dialog", accept, promptText: dialogReply || undefined });
    setDialog(null);
  };

  const pasteIntoBrowser = (text: string) => {
    if (!text) return;
    if (supportsPasteControl) {
      send({ type: "paste", text });
    } else {
      // Rolling upgrades and `tauri dev --no-watch` can put a new viewer in
      // front of a machine process that predates Control::Paste. Key controls
      // have always carried one printable character, so use that established
      // path until the machine advertises native paste support. Normalize line
      // endings first so CRLF does not become two newlines.
      for (const character of text.replace(/\r\n?/g, "\n")) {
        send({
          type: "key",
          event: "down",
          key: "",
          code: "",
          keyCode: 0,
          text: character === "\n" ? "\r" : character,
          modifiers: 0,
        });
      }
    }
    canvasRef.current?.focus();
  };

  const pasteFromClipboard = async () => {
    if (readingClipboard) return;
    setReadingClipboard(true);
    let text: string | null = null;
    try {
      if (isNativeShell()) {
        text = await readNativeClipboardText();
      } else if (window.isSecureContext && navigator.clipboard?.readText) {
        text = await navigator.clipboard.readText();
      }
    } catch {
      // LAN browsers and denied clipboard permissions use the manual sheet.
    } finally {
      setReadingClipboard(false);
    }

    if (text) {
      pasteIntoBrowser(text);
      return;
    }
    setPasteText("");
    setPasteOpen(true);
  };

  const submitManualPaste = () => {
    if (!pasteText) return;
    pasteIntoBrowser(pasteText);
    setPasteText("");
    setPasteOpen(false);
  };

  /** Right-click hit-test. Chrome is headless in another process, so only its
   *  own document knows what sits under the point; ask the backend and open the
   *  menu with the answer. Resolves to empty context (generic menu) if the
   *  socket is down or the probe does not answer promptly, so the menu never
   *  hangs. */
  const probeContext = (point: { x: number; y: number }): Promise<BrowserContext> => {
    const socket = wsRef.current;
    if (socket?.readyState !== WebSocket.OPEN) return Promise.resolve({});
    const nonce = `p${++probeSeq.current}`;
    return new Promise<BrowserContext>((resolve) => {
      const settle = (ctx: BrowserContext) => {
        probeWaiters.current.delete(nonce);
        resolve(ctx);
      };
      probeWaiters.current.set(nonce, settle);
      window.setTimeout(() => {
        if (probeWaiters.current.has(nonce)) settle({});
      }, 600);
      send({ type: "probe", ...point, nonce });
    });
  };

  /** Copy into the viewer's clipboard — Chrome's own clipboard is in a separate
   *  headless process the viewer can't read, so page "Copy" has to land here.
   *  Insecure LAN origins deny clipboard writes; there is no silent page-side
   *  fallback, so the item simply no-ops there, as paste already does. */
  const writeHostClipboard = async (text: string) => {
    if (!text) return;
    try {
      if (window.isSecureContext && navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(text);
      }
    } catch {
      // Denied or unavailable: nothing more we can safely do from here.
    }
  };

  /** Ctrl/Cmd+A to the page — select-all is an in-page action, so a key chord
   *  reaches it without any new backend primitive. */
  const selectAllInPage = () => {
    const modifiers = 2; // Ctrl bit in the shared mask (see modifierMask).
    send({ type: "key", event: "down", key: "a", code: "KeyA", keyCode: 65, modifiers });
    send({ type: "key", event: "up", key: "a", code: "KeyA", keyCode: 65, modifiers });
    canvasRef.current?.focus();
  };

  /** Cut = copy to the host clipboard, then let a Backspace clear the (still
   *  live, still focused) selection in the field the user right-clicked. Only
   *  offered over an editable element with a selection. */
  const cutSelection = async (text: string) => {
    await writeHostClipboard(text);
    send({ type: "key", event: "down", key: "Backspace", code: "Backspace", keyCode: 8, text: undefined, modifiers: 0 });
    send({ type: "key", event: "up", key: "Backspace", code: "Backspace", keyCode: 8, modifiers: 0 });
    canvasRef.current?.focus();
  };

  /** The right-click menu for one hit-test result. Contextual entries appear
   *  only when the point actually carries them, so a click on empty page gives
   *  the short navigation-and-select menu rather than a wall of dead items. */
  function menuItems(ctx: BrowserMenu): CtxItem[] {
    const items: CtxItem[] = [
      { label: "Back", onSelect: () => send({ type: "back" }) },
      { label: "Forward", onSelect: () => send({ type: "forward" }) },
      { label: "Reload", onSelect: () => send({ type: "reload" }) },
    ];
    if (ctx.linkHref) {
      items.push(
        { label: "Open link in new tab", dividerBefore: true, onSelect: () => send({ type: "newTab", url: ctx.linkHref }) },
        { label: "Copy link address", onSelect: () => void writeHostClipboard(ctx.linkHref ?? "") },
      );
    }
    if (ctx.imgSrc) {
      items.push(
        { label: "Open image in new tab", dividerBefore: true, onSelect: () => send({ type: "newTab", url: ctx.imgSrc }) },
        { label: "Copy image address", onSelect: () => void writeHostClipboard(ctx.imgSrc ?? "") },
      );
    }
    if (ctx.selection) {
      items.push({ label: "Copy", dividerBefore: true, onSelect: () => void writeHostClipboard(ctx.selection ?? "") });
    }
    if (ctx.editable && ctx.selection) {
      items.push({ label: "Cut", onSelect: () => void cutSelection(ctx.selection ?? "") });
    }
    if (ctx.editable) {
      items.push({ label: "Paste", dividerBefore: !ctx.selection, onSelect: () => void pasteFromClipboard() });
    }
    items.push({ label: "Select all", dividerBefore: true, onSelect: selectAllInPage });
    // Bitwarden, shaped by what the probe already knows — the whole point is
    // that the common case (unlocked, logins prefetched for this site) is one
    // click on a visible entry, not a sheet.
    const openSheet = () => {
      setVault({ ...EMPTY_SHEET, busy: true });
      setVaultPassword("");
      send({ type: "bwState" });
    };
    const bw = ctx.bitwarden;
    if (bw?.state === "unlocked" && bw.entries && bw.entries.length > 0) {
      // Every login saved for this site, directly in the menu. They are
      // already origin-filtered, so even a big vault yields a handful; the
      // shared ContextMenu clamps and scrolls if someone truly has dozens.
      bw.entries.forEach((entry, index) => {
        items.push({
          label: `Fill · ${entry.name}${entry.username ? ` (${entry.username})` : ""}`,
          dividerBefore: index === 0,
          onSelect: () => send({ type: "bwFill", id: entry.id }),
        });
      });
    } else if (bw?.state === "unlocked" && bw.entries && bw.entries.length === 0) {
      items.push({
        label: "Bitwarden: no login for this site",
        dividerBefore: true,
        onSelect: openSheet,
      });
    } else if (bw?.state === "locked") {
      const blocked = bw.requireKey && bw.keyPresent === false;
      items.push({
        label: blocked ? "Bitwarden: insert your security key" : "Unlock Bitwarden…",
        dividerBefore: true,
        onSelect: openSheet,
      });
    } else {
      // Not installed / logged out / an older machine process with no vault
      // info in the probe: the sheet explains whichever it is.
      items.push({
        label: "Fill login from Bitwarden…",
        dividerBefore: true,
        onSelect: openSheet,
      });
    }
    return items;
  }

  const toggleFullscreen = async () => {
    try {
      if (document.fullscreenElement === paneRef.current) {
        await document.exitFullscreen();
      } else {
        await paneRef.current?.requestFullscreen();
      }
    } catch {
      // A host may decline fullscreen (for example, a restricted embedded
      // browser). Leave the current view alone; the control remains usable if
      // the host's policy changes.
    }
  };

  return (
    <div className="browser-pane" ref={paneRef}>
      {attached && (
        <div className="browser-signin-note">
          <b>Signed in: {attached.name}</b>
          <span>
            {attached.origins.includes("*")
              ? "any site"
              : `limited to ${attached.origins.join(", ")}`}
            {" · sign in here yourself — Threadknot keeps the session, never your password"}
          </span>
        </div>
      )}
      <div className="browser-chrome">
        <button type="button" className="browser-btn" onClick={() => send({ type: "back" })} aria-label="Back" title="Back">
          <BackIcon />
        </button>
        <button type="button" className="browser-btn" onClick={() => send({ type: "forward" })} aria-label="Forward" title="Forward">
          <FwdIcon />
        </button>
        <button type="button" className="browser-btn" onClick={() => send({ type: "reload" })} aria-label="Reload" title="Reload">
          <ReloadIcon />
        </button>
        <button
          type="button"
          className={`browser-btn browser-fullscreen${fullscreen ? " on" : ""}`}
          onClick={() => void toggleFullscreen()}
          aria-label={fullscreen ? "Exit browser fullscreen" : "Enter browser fullscreen"}
          aria-pressed={fullscreen}
          title={fullscreen ? "Exit fullscreen (Esc)" : "Enter fullscreen"}
        >
          {fullscreen ? <FullscreenExitIcon /> : <FullscreenIcon />}
        </button>

        <input
          className="browser-url"
          type="text"
          inputMode="url"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          placeholder="Enter a URL (e.g. localhost:5173)"
          value={editing ? addr : url}
          onFocus={(event) => {
            setEditing(true);
            setAddr(url);
            event.currentTarget.select();
          }}
          onChange={(event) => {
            setEditing(true);
            setAddr(event.target.value);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              navigate(addr);
              event.currentTarget.blur();
            } else if (event.key === "Escape") {
              event.preventDefault();
              setEditing(false);
              setAddr(url);
              event.currentTarget.blur();
            }
          }}
          onBlur={() => setEditing(false)}
        />

        <span className={`browser-connection ${connection}`} title={`Browser engine: ${connection}`}>
          <span className="browser-connection-dot" />
        </span>

        <div className="browser-more" ref={moreRef}>
          <button
            type="button"
            className={`browser-btn browser-more-toggle${moreOpen ? " on" : ""}`}
            onClick={() => setMoreOpen((open) => !open)}
            aria-label="More browser controls"
            aria-expanded={moreOpen}
            aria-haspopup="dialog"
            title="More browser controls"
          >
            <MoreIcon />
            {activities.length > 0 && !activityOpen && <span className="browser-activity-count">{activities.length}</span>}
          </button>

          {moreOpen && (
            <div className="browser-more-menu" role="dialog" aria-label="More browser controls">
              <div className="browser-more-section">
                <button
                  type="button"
                  className={`browser-more-action${activityOpen ? " on" : ""}`}
                  onClick={() => {
                    setActivityOpen((open) => !open);
                    setMoreOpen(false);
                  }}
                >
                  <ActivityIcon />
                  <span>Agent activity</span>
                  {activities.length > 0 && <em>{activities.length}</em>}
                </button>
                <button
                  type="button"
                  className={`browser-more-action${readingClipboard ? " reading" : ""}`}
                  onClick={() => {
                    void pasteFromClipboard();
                    setMoreOpen(false);
                  }}
                  disabled={readingClipboard || connection !== "live"}
                >
                  <PasteIcon />
                  <span>{readingClipboard ? "Reading clipboard…" : "Paste into browser"}</span>
                </button>
              </div>

              <div className="browser-more-section browser-more-fields">
                {thread && (
                  <label>
                    <span>Browser profile</span>
                    <select
                      className={attached ? "on" : ""}
                      value={attachedId}
                      onChange={(event) => attachProfile(event.target.value)}
                      aria-label="Signed-in browser profile"
                      title={profileTitle}
                    >
                      <option value="">Disposable browser</option>
                      {profilesLoaded && attachedId && !attached && (
                        <option value={attachedId}>Signed-in profile missing (deleted)</option>
                      )}
                      {profiles.map((profile) => (
                        <option key={profile.id} value={profile.id}>
                          Signed in: {profile.name}
                        </option>
                      ))}
                    </select>
                  </label>
                )}
                <label>
                  <span>Viewport</span>
                  <select value={device} onChange={(event) => setDevice(event.target.value)} aria-label="Device size">
                    {DEVICES.map((preset) => (
                      <option key={preset.id} value={preset.id}>
                        {preset.label}
                      </option>
                    ))}
                  </select>
                </label>
              </div>

              <div className="browser-more-section">
                <a
                  className={`browser-more-action${url ? "" : " disabled"}`}
                  href={url || undefined}
                  target="_blank"
                  rel="noreferrer"
                  aria-disabled={!url}
                  onClick={(event) => {
                    if (!url) event.preventDefault();
                    else setMoreOpen(false);
                  }}
                >
                  <ExternalIcon />
                  <span>Open in system browser</span>
                </a>
                <button
                  type="button"
                  className="browser-more-action"
                  onClick={() => {
                    restartBrowser();
                    setMoreOpen(false);
                  }}
                >
                  <PowerIcon />
                  <span>Restart isolated browser</span>
                </button>
              </div>
            </div>
          )}
        </div>
      </div>

      <div className="browser-tabs" role="tablist" aria-label="Browser tabs">
        <div className="browser-tabs-scroll">
          {tabs.map((tab) => (
            <div key={tab.id} className={`browser-tab${tab.active ? " active" : ""}`}>
              <button
                type="button"
                role="tab"
                aria-selected={tab.active}
                className="browser-tab-main"
                title={tab.url}
                onClick={() => {
                  if (!tab.active) send({ type: "switchTab", id: tab.id });
                }}
              >
                <span className="browser-tab-state" />
                <span className="browser-tab-title">{tabLabel(tab)}</span>
              </button>
              <button
                type="button"
                className="browser-tab-close"
                aria-label={`Close ${tabLabel(tab)}`}
                onClick={() => send({ type: "closeTab", id: tab.id })}
              >
                ×
              </button>
            </div>
          ))}
        </div>
        <button
          type="button"
          className="browser-new-tab"
          aria-label="New browser tab"
          title="New browser tab"
          onClick={() => send({ type: "newTab" })}
        >
          +
        </button>
      </div>

      <div className="browser-body">
        <div className="browser-stage" ref={stageRef}>
          <canvas
            ref={canvasRef}
            className="browser-canvas"
            tabIndex={0}
            aria-label="Shared browser viewport"
            onPointerDown={(event) => {
              event.preventDefault();
              event.currentTarget.setPointerCapture?.(event.pointerId);
              event.currentTarget.focus();
              const point = toFrameCoords(event);
              const button = buttonName(event.button);
              // Dismissing the right-click menu has to happen here. ContextMenu
              // watches for an outside `mousedown`, and the preventDefault above
              // suppresses the compatibility mouse events — the same reason the
              // menu opens from pointerup rather than `contextmenu`. As in
              // Chrome, the press that closes the menu is consumed by the
              // dismissal and never reaches the page; a right press is the
              // exception, since it re-opens the menu where you just clicked.
              if (menu) {
                setMenu(null);
                if (button !== "right") return;
              }
              pressedButton.current = button;
              send({ type: "mouse", event: "pressed", ...point, button, clickCount: event.detail || 1 });
            }}
            onPointerUp={releasePointer}
            onPointerCancel={releasePointer}
            onPointerMove={(event) => {
              if (!event.isPrimary) return;
              queuePointerMove(toFrameCoords(event));
            }}
            // Kept only to suppress the host webview's own menu; the pane's menu
            // is opened from the right-button pointerup (see releasePointer),
            // because preventDefault on pointerdown stops this event firing.
            onContextMenu={(event) => event.preventDefault()}
            onWheel={(event) => {
              // Ctrl/Cmd+wheel is pane zoom, not page scroll: let it bubble to
              // the window-level hotwheel handler untouched. (Mac pinch also
              // arrives here as ctrl+wheel.)
              if (event.ctrlKey || event.metaKey) return;
              event.preventDefault();
              const point = toFrameCoords(event);
              send({ type: "wheel", ...point, deltaX: event.deltaX, deltaY: event.deltaY });
            }}
            onKeyDown={(event) => {
              // Chrome is a separate headless process, so forwarding Ctrl/Cmd+V
              // would ask it to read a clipboard it cannot see. Do not cancel
              // the host shortcut: it fires `onPaste` below with the real text.
              if (isPasteShortcut(event)) return;
              event.preventDefault();
              const printable = event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey;
              send({
                type: "key",
                event: "down",
                key: event.key,
                code: event.code,
                keyCode: event.keyCode,
                text: printable ? event.key : undefined,
                modifiers: modifierMask(event),
              });
            }}
            onKeyUp={(event) => {
              if (isPasteShortcut(event)) return;
              event.preventDefault();
              send({
                type: "key",
                event: "up",
                key: event.key,
                code: event.code,
                keyCode: event.keyCode,
                modifiers: modifierMask(event),
              });
            }}
            onPaste={(event) => {
              event.preventDefault();
              const text = event.clipboardData.getData("text/plain");
              pasteIntoBrowser(text);
            }}
          />

          {latestActivity && (
            <div className={`browser-agent-hud ${latestActivity.phase}`} role="status" aria-live="polite">
              <span className="browser-agent-mark">AGENT</span>
              <span>{latestActivity.label}</span>
              {latestActivity.phase === "failed" && <span className="browser-agent-failed">Failed</span>}
            </div>
          )}

          {activityBounds && (
            <div
              className={`browser-agent-target ${latestActivity?.phase ?? ""}`}
              style={activityBounds}
              aria-hidden="true"
            />
          )}
          {activityPoint && (
            <div
              className={`browser-agent-cursor ${latestActivity?.phase ?? ""}`}
              style={{ left: activityPoint.left, top: activityPoint.top }}
              aria-hidden="true"
            >
              <CursorIcon />
              <span>Agent</span>
            </div>
          )}

          {activityOpen && (
            <aside className="browser-activity-panel" aria-label="Agent browser activity">
              <header>
                <div>
                  <span className="browser-eyebrow">SHARED SESSION</span>
                  <h3>Agent activity</h3>
                </div>
                <button type="button" className="browser-panel-close" onClick={() => setActivityOpen(false)} aria-label="Close activity">
                  ×
                </button>
              </header>
              <div className="browser-activity-list">
                {activities.length === 0 ? (
                  <p className="browser-activity-empty">Agent browser actions will appear here in real time.</p>
                ) : (
                  [...activities].reverse().map((activity) => (
                    <div key={activity.id} className={`browser-activity-row ${activity.phase}`}>
                      <span className="browser-activity-status" />
                      <div>
                        <strong>{activity.label}</strong>
                        {activity.detail && <p>{activity.detail}</p>}
                      </div>
                      <time dateTime={activity.ts}>{activityTime(activity.ts)}</time>
                    </div>
                  ))
                )}
              </div>
            </aside>
          )}

          {dialog && (
            <div className="browser-dialog-shade">
              <section className="browser-dialog-card" role="alertdialog" aria-modal="true" aria-label={`${dialog.kind} from page`}>
                <span className="browser-eyebrow">PAGE {dialog.kind.toUpperCase()}</span>
                <p>{dialog.message}</p>
                {dialog.kind === "prompt" && (
                  <input
                    type="text"
                    value={dialogReply}
                    autoFocus
                    onChange={(event) => setDialogReply(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") answerDialog(true);
                      if (event.key === "Escape") answerDialog(false);
                    }}
                  />
                )}
                <div className="browser-dialog-actions">
                  <button type="button" onClick={() => answerDialog(false)}>Dismiss</button>
                  <button type="button" className="primary" onClick={() => answerDialog(true)}>Accept</button>
                </div>
              </section>
            </div>
          )}

          {pasteOpen && (
            <div className="browser-dialog-shade browser-paste-shade">
              <form
                className="browser-dialog-card browser-paste-card"
                role="dialog"
                aria-modal="true"
                aria-labelledby="browser-paste-title"
                onSubmit={(event) => {
                  event.preventDefault();
                  submitManualPaste();
                }}
              >
                <span className="browser-eyebrow">CLIPBOARD FALLBACK</span>
                <label id="browser-paste-title" htmlFor={`browser-paste-${sessionId}`}>
                  Paste text here
                  <span>Touch and hold in the box, then choose Paste.</span>
                </label>
                <textarea
                  id={`browser-paste-${sessionId}`}
                  value={pasteText}
                  autoFocus
                  rows={4}
                  placeholder="Paste from your phone’s clipboard"
                  onChange={(event) => setPasteText(event.currentTarget.value)}
                />
                <div className="browser-dialog-actions">
                  <button
                    type="button"
                    onClick={() => {
                      setPasteText("");
                      setPasteOpen(false);
                      canvasRef.current?.focus();
                    }}
                  >
                    Cancel
                  </button>
                  <button type="submit" className="primary" disabled={!pasteText}>
                    Paste into browser
                  </button>
                </div>
              </form>
            </div>
          )}

          {vault && (
            <div className="browser-dialog-shade">
              <form
                className="browser-dialog-card browser-vault-card"
                role="dialog"
                aria-modal="true"
                aria-labelledby="browser-vault-title"
                onSubmit={(event) => {
                  event.preventDefault();
                  if (!vaultPassword || vault.busy) return;
                  setVault({ ...vault, busy: true, error: null });
                  // The reply carries this site's logins; exactly one means the
                  // unlock IS the fill — no picker with a single option.
                  autoFillRef.current = true;
                  send({ type: "bwUnlock", password: vaultPassword });
                  // Dropped from this component the moment it is sent; it lives
                  // only long enough to reach the backend.
                  setVaultPassword("");
                }}
              >
                <span className="browser-eyebrow">BITWARDEN</span>

                {vault.state === "notInstalled" && (
                  <>
                    <h3 id="browser-vault-title" className="browser-vault-heading">
                      Bitwarden isn’t installed
                    </h3>
                    <p className="browser-vault-sub">
                      Install the CLI with <code>npm install -g @bitwarden/cli</code>, then try
                      again.
                    </p>
                  </>
                )}

                {vault.state === "loggedOut" && (
                  <>
                    <h3 id="browser-vault-title" className="browser-vault-heading">
                      Sign in to Bitwarden first
                    </h3>
                    <p className="browser-vault-sub">
                      Run <code>bw login</code> once in a terminal — it needs your email and
                      two-factor code, so it isn’t done from here. After that, unlocking
                      happens right here.
                    </p>
                  </>
                )}

                {vault.state === "locked" && (
                  <>
                    <h3 id="browser-vault-title" className="browser-vault-heading">
                      Unlock your vault
                    </h3>
                    <p className="browser-vault-sub">
                      Stays unlocked for 12 hours on this machine. Your master password is
                      never written to disk.
                    </p>
                    <input
                      id={`browser-vault-${sessionId}`}
                      className="browser-vault-pass"
                      type="password"
                      autoFocus
                      autoComplete="off"
                      placeholder="Master password"
                      aria-label="Master password"
                      value={vaultPassword}
                      disabled={vault.busy || (vault.requireKey && !vault.keyPresent)}
                      onChange={(event) => setVaultPassword(event.currentTarget.value)}
                    />
                    {vault.requireKey && !vault.keyPresent && (
                      <p className="browser-vault-keynote">
                        Your security key is not inserted — the vault stays locked without it.
                      </p>
                    )}
                  </>
                )}

                {vault.state === "unlocked" && (
                  <>
                    <h3 id="browser-vault-title" className="browser-vault-heading">
                      {vault.busy
                        ? "Looking for matching logins…"
                        : vault.items.length === 0
                          ? "No saved login for this site"
                          : "Choose a login"}
                    </h3>
                    <div className="browser-vault-list">
                      {vault.items.map((item) => (
                        <button
                          key={item.id}
                          type="button"
                          className="browser-vault-item"
                          onClick={() => {
                            setVault({ ...vault, busy: true, error: null });
                            send({ type: "bwFill", id: item.id });
                          }}
                        >
                          <span className="browser-vault-name">{item.name}</span>
                          <span className="browser-vault-user">{item.username || "no username"}</span>
                        </button>
                      ))}
                    </div>
                  </>
                )}

                {(vault.state === "locked" || vault.state === "unlocked") && (
                  // The physical gate, drawn as the thing it is: a USB security
                  // key. The whole row toggles it. Shown with live presence, so
                  // turning it on with no key inserted (= locking yourself out
                  // until you find it) is a visible choice, not an accident.
                  <button
                    type="button"
                    className={`browser-vault-keytoggle${vault.requireKey ? " on" : ""}${vault.keyPresent ? " present" : ""}`}
                    aria-pressed={vault.requireKey}
                    disabled={vault.busy || (!vault.requireKey && !vault.keyPresent)}
                    onClick={() => send({ type: "bwRequireKey", on: !vault.requireKey })}
                  >
                    <UsbKeyIcon />
                    <span className="browser-vault-keycopy">
                      <strong>Require my security key</strong>
                      <em>
                        {vault.keyPresent
                          ? "key inserted"
                          : "no key detected — plug it in to enable"}
                        {vault.requireKey ? " · removing it locks the vault" : ""}
                      </em>
                    </span>
                    <span className="browser-vault-keystate">{vault.requireKey ? "ON" : "OFF"}</span>
                  </button>
                )}

                {vault.error && <div className="modal-error">{vault.error}</div>}

                <div className="browser-dialog-actions">
                  <button
                    type="button"
                    onClick={() => {
                      setVault(null);
                      setVaultPassword("");
                      canvasRef.current?.focus();
                    }}
                  >
                    Close
                  </button>
                  {vault.state === "locked" && (
                    <button type="submit" className="primary" disabled={!vaultPassword || vault.busy}>
                      {vault.busy ? "Unlocking…" : "Unlock"}
                    </button>
                  )}
                  {vault.state === "unlocked" && (
                    <button
                      type="button"
                      onClick={() => {
                        send({ type: "bwLock" });
                        setVault(null);
                      }}
                    >
                      Lock vault
                    </button>
                  )}
                </div>
              </form>
            </div>
          )}

          {!url && (
            <div className="browser-empty">
              <div className="browser-empty-head">
                <div>
                  <span className="browser-eyebrow">LOCAL DISCOVERY</span>
                  <h3>Running dev servers</h3>
                </div>
                <button type="button" className="browser-rescan" onClick={() => void rescan()} disabled={scanning}>
                  {scanning ? "Scanning…" : "Rescan"}
                </button>
              </div>

              {ports.length > 0 ? (
                <div className="browser-ports">
                  {ports.map((port) => (
                    <button key={port.port} type="button" className="browser-port" onClick={() => navigate(port.url)}>
                      <span className="browser-port-num">:{port.port}</span>
                      <span className="browser-port-title">{port.title || port.url}</span>
                    </button>
                  ))}
                </div>
              ) : (
                scanned &&
                !scanning && (
                  <p className="browser-empty-hint">
                    No dev servers found. Start your dev server, then rescan — or type a URL above.
                  </p>
                )
              )}
              {connection !== "live" && <p className="browser-empty-hint">Connecting to the isolated browser engine…</p>}
            </div>
          )}
        </div>
      </div>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={menuItems(menu)}
          onClose={() => setMenu(null)}
        />
      )}
    </div>
  );
}

// ---- inline icons (self-contained) ----

const svgProps = {
  width: 18,
  height: 18,
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 2,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  "aria-hidden": true,
};

/** A USB-A security key, drawn on its side: connector (with its two contact
 *  slots), body, touch disc, and the keyring hole every Thetis/YubiKey has.
 *  Sized by CSS on the vault sheet's toggle row. */
const UsbKeyIcon = () => (
  <svg
    {...svgProps}
    width={40}
    height={40}
    viewBox="0 0 48 24"
    strokeWidth={1.6}
    className="browser-vault-keyicon"
  >
    {/* connector shell */}
    <rect x="2" y="7" width="10" height="10" rx="1" />
    {/* contact slots */}
    <path d="M5 10h4M5 14h4" />
    {/* body */}
    <rect x="12" y="4.5" width="28" height="15" rx="3.5" />
    {/* touch disc */}
    <circle cx="27" cy="12" r="4" />
    <circle cx="27" cy="12" r="1.2" fill="currentColor" stroke="none" />
    {/* keyring hole */}
    <circle cx="36.5" cy="12" r="1.8" />
  </svg>
);

const BackIcon = () => (
  <svg {...svgProps}>
    <path d="M15 18l-6-6 6-6" />
  </svg>
);
const FwdIcon = () => (
  <svg {...svgProps}>
    <path d="M9 18l6-6-6-6" />
  </svg>
);
const ReloadIcon = () => (
  <svg {...svgProps}>
    <path d="M21 12a9 9 0 1 1-2.64-6.36" />
    <path d="M21 3v6h-6" />
  </svg>
);
const ExternalIcon = () => (
  <svg {...svgProps}>
    <path d="M14 4h6v6" />
    <path d="M20 4l-8 8" />
    <path d="M18 13v5a2 2 0 0 1-2 2H6a2 2 0 0 1 2-2V8a2 2 0 0 1 2-2h5" />
  </svg>
);
const FullscreenIcon = () => (
  <svg {...svgProps}>
    <path d="M8 4H4v4M16 4h4v4M20 16v4h-4M4 16v4h4" />
  </svg>
);
const FullscreenExitIcon = () => (
  <svg {...svgProps}>
    <path d="M8 4v4H4M16 4v4h4M20 16h-4v4M4 16h4v4" />
  </svg>
);
const MoreIcon = () => (
  <svg {...svgProps}>
    <circle cx="12" cy="5" r="1" fill="currentColor" stroke="none" />
    <circle cx="12" cy="12" r="1" fill="currentColor" stroke="none" />
    <circle cx="12" cy="19" r="1" fill="currentColor" stroke="none" />
  </svg>
);
const ActivityIcon = () => (
  <svg {...svgProps}>
    <path d="M4 14h3l2-7 4 12 2-5h5" />
  </svg>
);
const PasteIcon = () => (
  <svg {...svgProps}>
    <path d="M9 5h6" />
    <path d="M9 3h6a1 1 0 0 1 1 1v3H8V4a1 1 0 0 1 1-1z" />
    <path d="M8 5H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2h-2" />
    <path d="M12 11v6m-3-3 3 3 3-3" />
  </svg>
);
const PowerIcon = () => (
  <svg {...svgProps}>
    <path d="M12 2v10" />
    <path d="M18.4 6.6a8 8 0 1 1-12.8 0" />
  </svg>
);
const CursorIcon = () => (
  <svg width="22" height="26" viewBox="0 0 22 26" fill="none" aria-hidden="true">
    <path d="M2 2l17 11-8 1.5L7 23 2 2z" fill="currentColor" stroke="#091017" strokeWidth="1.5" strokeLinejoin="round" />
  </svg>
);
