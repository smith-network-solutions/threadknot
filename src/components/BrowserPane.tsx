import { useCallback, useEffect, useRef, useState } from "react";
import type { BrowserProfileInfo, PortInfo, Project } from "../lib/protocol";
import { findThread, useStore } from "../state/store";
import { browserWsUrl } from "../lib/discovery";
import { isNativeShell, readNativeClipboardText } from "../lib/native";
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
  project: _project,
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

  const stageRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const frameSize = useRef({ w: 0, h: 0 });
  const editingRef = useRef(false);
  const pointerFrame = useRef<number | null>(null);
  const pendingPointer = useRef<{ x: number; y: number } | null>(null);
  const pressedButton = useRef<string | null>(null);

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

  const [ports, setPorts] = useState<PortInfo[]>([]);
  const [scanning, setScanning] = useState(false);
  const [scanned, setScanned] = useState(false);

  useEffect(() => {
    editingRef.current = editing;
  }, [editing]);

  useEffect(() => {
    if (active) setEnabled(true);
  }, [active]);

  useEffect(() => {
    setDialogReply(dialog?.defaultPrompt ?? "");
  }, [dialog]);

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
    const observer = new ResizeObserver(() => setLayoutRevision((value) => value + 1));
    observer.observe(stage);
    return () => observer.disconnect();
  }, [enabled]);

  useEffect(
    () => () => {
      if (pointerFrame.current != null) cancelAnimationFrame(pointerFrame.current);
    },
    [],
  );

  // Push device-size changes to the backend viewport.
  useEffect(() => {
    if (connection !== "live") return;
    const preset = DEVICES.find((item) => item.id === device) ?? DEVICES[0];
    if (preset.width && preset.height) {
      send({ type: "resize", width: preset.width, height: preset.height });
    } else {
      send({ type: "resize", width: 1280, height: 800 });
    }
  }, [connection, device, send]);

  function toFrameCoords(event: React.PointerEvent | React.WheelEvent): { x: number; y: number } {
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

  return (
    <div className="browser-pane">
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
          <span className="browser-connection-label">{connection === "live" ? "Live" : connection === "retrying" ? "Retrying" : "Connecting"}</span>
        </span>

        <button
          type="button"
          className={`browser-btn browser-activity-toggle${activityOpen ? " on" : ""}`}
          onClick={() => setActivityOpen((open) => !open)}
          aria-label="Agent browser activity"
          aria-expanded={activityOpen}
          title="Agent browser activity"
        >
          <ActivityIcon />
          {activities.length > 0 && <span className="browser-activity-count">{activities.length}</span>}
        </button>

        <button
          type="button"
          className={`browser-btn browser-paste-btn${readingClipboard ? " reading" : ""}`}
          onClick={() => void pasteFromClipboard()}
          disabled={readingClipboard || connection !== "live"}
          aria-label={readingClipboard ? "Reading clipboard" : "Paste into browser"}
          title={readingClipboard ? "Reading clipboard…" : "Paste into the focused browser field"}
        >
          <PasteIcon />
        </button>

        {thread && (
          <select
            className={`browser-profile${attached ? " on" : ""}`}
            value={attachedId}
            onChange={(event) => attachProfile(event.target.value)}
            aria-label="Signed-in browser profile"
            title={
              attached
                ? attached.origins.includes("*")
                  ? `Signed in as ${attached.name} — this browser keeps the login and may visit any site`
                  : `Signed in as ${attached.name} — this browser may only visit ${attached.origins.join(", ")}`
                : profilesLoaded && attachedId
                  ? "This chat points at a signed-in profile that no longer exists (it was deleted). Switch to Disposable, or attach another profile."
                  : "Disposable browser: nothing stays signed in. Pick a profile to keep a login."
            }
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
        )}

        <select
          className="browser-device"
          value={device}
          onChange={(event) => setDevice(event.target.value)}
          aria-label="Device size"
          title="Device size"
        >
          {DEVICES.map((preset) => (
            <option key={preset.id} value={preset.id}>
              {preset.label}
            </option>
          ))}
        </select>

        <button
          type="button"
          className="browser-btn"
          onClick={restartBrowser}
          aria-label="Restart isolated browser"
          title="Restart isolated browser"
        >
          <PowerIcon />
        </button>

        <a
          className="browser-btn browser-ext"
          href={url || undefined}
          target="_blank"
          rel="noreferrer"
          aria-label="Open in system browser"
          title="Open in system browser"
          aria-disabled={!url}
          onClick={(event) => {
            if (!url) event.preventDefault();
          }}
        >
          <ExternalIcon />
        </a>
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
              pressedButton.current = button;
              send({ type: "mouse", event: "pressed", ...point, button, clickCount: event.detail || 1 });
            }}
            onPointerUp={releasePointer}
            onPointerCancel={releasePointer}
            onPointerMove={(event) => {
              if (!event.isPrimary) return;
              queuePointerMove(toFrameCoords(event));
            }}
            onContextMenu={(event) => event.preventDefault()}
            onWheel={(event) => {
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
