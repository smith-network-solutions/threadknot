import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import type {
  ArchiveHeader,
  BrowserProfileInfo,
  ConnectorStatus,
  DeviceCapability,
  DiscoveredPeer,
  RemoteAccess,
  UpdateStatus,
} from "../lib/protocol";
import {
  DEFAULT_DEVICE_CAPABILITIES,
  DEVICE_CAPABILITY_LABELS,
} from "../lib/protocol";
import { copyText, formatFullDateTime, timeAgo } from "../lib/format";
import { useIsMobile } from "../lib/viewport";
import { pickAvatarImage } from "../lib/sidebarImage";
import { MachineAvatar, machineLook } from "./MachineAvatar";
import { useAvatarHoverPreview } from "./AvatarHoverPreview";
import { hermesPresence } from "./HermesPresence";
import {
  getHermesEnabled,
  isAgentVisible,
  setHermesEnabled,
} from "../lib/agentVisibility";
import { CustomizeProfileModal } from "./CustomizeProfileModal";
import { ConfirmRemoveMachineModal } from "./ConfirmRemoveMachineModal";
import { PairPhoneModal } from "./PairPhoneModal";
import { DirPicker } from "./DirPicker";
import { LibrarySettings } from "./LibrarySettings";
import { AboutSettings } from "./AboutSettings";
import { LegacyCircuit } from "./legacy/LegacyCircuit";
import {
  chime,
  getNotifyPrefs,
  isWorkspaceSubscribed,
  requestWebPermission,
  setNotifyPrefs,
  testNativeNotification,
  webNotifyState,
  type NotifyScope,
} from "../lib/notify";
import { useStore } from "../state/store";
import {
  AUTOSETTLE_DEFAULT,
  AUTOSETTLE_MAX,
  AUTOSETTLE_MIN,
  CFONT_MAX,
  CFONT_MIN,
  clamp,
  COMPOSER_DENSITIES,
  COMPOSER_WIDTHS,
  getComposerPrefs,
  getSidebarPrefs,
  PROJECT_LAYOUTS,
  getTermPrefs,
  monoFontStack,
  setComposerPrefs,
  setSidebarPrefs,
  setTermPrefs,
  SCROLLBACK_MAX,
  SCROLLBACK_MIN,
  TFONT_MAX,
  TFONT_MIN,
  type ComposerDensity,
  type ComposerPrefs,
  type ComposerWidth,
  type ProjectLayout,
  type SidebarPrefs,
  type TermPrefs,
} from "../lib/appearance";
import { AppearanceStudio, ThemeSync } from "./ThemeStudio";
import { SkinsSettings } from "./SkinsSettings";
import { FontPicker } from "./FontPicker";
import {
  AgentMark,
  ChevronIcon,
  CopyIcon,
  CheckIcon,
  GlobeIcon,
  MoreIcon,
  PencilIcon,
  TrashIcon,
  UsbKeyIcon,
  XIcon,
} from "./icons";
import type {
  ClaudexProfileInfo,
  ClaudexProfileInput,
  DictationProvider,
  DictationSettings,
  HermesAgentDetails,
  HermesAgentInfo,
} from "../lib/protocol";

function Stepper({
  label,
  value,
  onDec,
  onInc,
  decDisabled,
  incDisabled,
}: {
  label: string;
  value: string;
  onDec: () => void;
  onInc: () => void;
  decDisabled: boolean;
  incDisabled: boolean;
}) {
  return (
    <div className="settings-row">
      <span className="settings-value">{label}</span>
      <span className="settings-step">
        <button
          type="button"
          className="settings-step-btn"
          aria-label={`Decrease ${label}`}
          onClick={onDec}
          disabled={decDisabled}
        >
          −
        </button>
        <span className="settings-step-val">{value}</span>
        <button
          type="button"
          className="settings-step-btn"
          aria-label={`Increase ${label}`}
          onClick={onInc}
          disabled={incDisabled}
        >
          +
        </button>
      </span>
    </div>
  );
}

const PROJECT_LAYOUT_LABELS: Record<ProjectLayout, string> = {
  sections: "all open",
  accordion: "one open",
  picker: "picker",
  rail: "rail",
};

const PROJECT_LAYOUT_HINTS: Record<ProjectLayout, string> = {
  sections: "Every project expanded at once — the original layout.",
  accordion: "Opening a project closes the others. All the headers stay visible, but only one list of chats is ever on screen.",
  picker: "One project at a time, chosen from a dropdown at the top. No headers, so the chat list gets the whole sidebar.",
  rail: "A column of project icons down the left edge. One tap to switch, every project always in view with its own unread badge.",
};

function SidebarSettings() {
  const [s, setS] = useState(getSidebarPrefs);
  function update(next: SidebarPrefs) {
    setS(next);
    setSidebarPrefs(next);
  }
  const days = s.autoSettleDays;
  return (
    <div className="settings-block">
      <div className="settings-label">sidebar</div>
      <div className="settings-row">
        <span className="settings-value">
          projects
          <span className="settings-hint">
            {PROJECT_LAYOUT_HINTS[s.projectLayout]}
          </span>
        </span>
        <span className="settings-seg">
          {PROJECT_LAYOUTS.map((layout) => (
            <button
              key={layout}
              type="button"
              className={`settings-toggle ${s.projectLayout === layout ? "on" : ""}`}
              onClick={() => update({ ...s, projectLayout: layout })}
            >
              {PROJECT_LAYOUT_LABELS[layout]}
            </button>
          ))}
        </span>
      </div>
      <div className="settings-row">
        <span className="settings-value">
          settle quiet chats
          <span className="settings-hint">
            Chats with no activity for this long move to the settled shelf on their own.
            Anything still working, waiting on you, or unread stays put.
          </span>
        </span>
        <button
          type="button"
          className={`settings-toggle ${days !== null ? "on" : ""}`}
          onClick={() =>
            update({ ...s, autoSettleDays: days === null ? AUTOSETTLE_DEFAULT : null })
          }
        >
          {days !== null ? "on" : "off"}
        </button>
      </div>
      {days !== null && (
        <Stepper
          label="after"
          value={days === 1 ? "1 day" : `${days} days`}
          onDec={() =>
            update({ ...s, autoSettleDays: clamp(days - 1, AUTOSETTLE_MIN, AUTOSETTLE_MAX) })
          }
          onInc={() =>
            update({ ...s, autoSettleDays: clamp(days + 1, AUTOSETTLE_MIN, AUTOSETTLE_MAX) })
          }
          decDisabled={days <= AUTOSETTLE_MIN}
          incDisabled={days >= AUTOSETTLE_MAX}
        />
      )}
    </div>
  );
}

const COMPOSER_WIDTH_HINTS: Record<ComposerWidth, string> = {
  cozy: "A 983px column aligned to the conversation's content limit.",
  wide: "A roomier 1040px column, so long prompts wrap less often.",
  full: "Edge to edge: the box takes whatever width the pane has.",
};

const COMPOSER_DENSITY_HINTS: Record<ComposerDensity, string> = {
  comfortable: "The original padding around the message box.",
  compact: "Tighter padding, handing the height back to the conversation above.",
};

/** The message box sizes itself now that zoom scales only the message feed, so
 *  these three knobs are the whole story. Every one of them writes a CSS var on
 *  :root, so the composer behind this panel changes as the buttons are pressed. */
function ComposerSettings() {
  const [c, setC] = useState(getComposerPrefs);
  function update(next: ComposerPrefs) {
    setC(next);
    setComposerPrefs(next);
  }
  return (
    <div className="settings-block">
      <div className="settings-label">composer</div>
      <div className="settings-row">
        <span className="settings-value">
          width
          <span className="settings-hint">{COMPOSER_WIDTH_HINTS[c.width]}</span>
        </span>
        <span className="settings-seg">
          {COMPOSER_WIDTHS.map((w) => (
            <button
              key={w}
              type="button"
              className={`settings-toggle ${c.width === w ? "on" : ""}`}
              onClick={() => update({ ...c, width: w })}
            >
              {w}
            </button>
          ))}
        </span>
      </div>
      <Stepper
        label="text size"
        value={`${c.fontSize}px`}
        onDec={() => update({ ...c, fontSize: clamp(c.fontSize - 1, CFONT_MIN, CFONT_MAX) })}
        onInc={() => update({ ...c, fontSize: clamp(c.fontSize + 1, CFONT_MIN, CFONT_MAX) })}
        decDisabled={c.fontSize <= CFONT_MIN}
        incDisabled={c.fontSize >= CFONT_MAX}
      />
      <div className="settings-row">
        <span className="settings-value">
          density
          <span className="settings-hint">{COMPOSER_DENSITY_HINTS[c.density]}</span>
        </span>
        <span className="settings-seg">
          {COMPOSER_DENSITIES.map((d) => (
            <button
              key={d}
              type="button"
              className={`settings-toggle ${c.density === d ? "on" : ""}`}
              onClick={() => update({ ...c, density: d })}
            >
              {d}
            </button>
          ))}
        </span>
      </div>
    </div>
  );
}

function TerminalSettings() {
  const [t, setT] = useState(getTermPrefs);
  function update(next: TermPrefs) {
    setT(next);
    setTermPrefs(next);
  }
  return (
    <div className="settings-block">
      <div className="settings-label">terminal</div>
      <Stepper
        label="font size"
        value={`${t.fontSize}px`}
        onDec={() => update({ ...t, fontSize: clamp(t.fontSize - 1, TFONT_MIN, TFONT_MAX) })}
        onInc={() => update({ ...t, fontSize: clamp(t.fontSize + 1, TFONT_MIN, TFONT_MAX) })}
        decDisabled={t.fontSize <= TFONT_MIN}
        incDisabled={t.fontSize >= TFONT_MAX}
      />
      <div className="settings-row">
        <span className="settings-value">cursor</span>
        <span className="settings-seg">
          {(["bar", "block", "underline"] as const).map((cs) => (
            <button
              key={cs}
              type="button"
              className={`settings-toggle ${t.cursorStyle === cs ? "on" : ""}`}
              onClick={() => update({ ...t, cursorStyle: cs })}
            >
              {cs}
            </button>
          ))}
        </span>
      </div>
      <div className="settings-row">
        <span className="settings-value">cursor blink</span>
        <button
          type="button"
          className={`settings-toggle ${t.cursorBlink ? "on" : ""}`}
          onClick={() => update({ ...t, cursorBlink: !t.cursorBlink })}
        >
          {t.cursorBlink ? "on" : "off"}
        </button>
      </div>
      <Stepper
        label="scrollback"
        value={t.scrollback.toLocaleString()}
        onDec={() => update({ ...t, scrollback: clamp(t.scrollback - 5000, SCROLLBACK_MIN, SCROLLBACK_MAX) })}
        onInc={() => update({ ...t, scrollback: clamp(t.scrollback + 5000, SCROLLBACK_MIN, SCROLLBACK_MAX) })}
        decDisabled={t.scrollback <= SCROLLBACK_MIN}
        incDisabled={t.scrollback >= SCROLLBACK_MAX}
      />
      <div className="settings-row theme-font-row">
        <span className="settings-value">
          font
          <span className="theme-font-preview" style={{ fontFamily: monoFontStack(t.fontFamily) }}>
            const x = 42;
          </span>
        </span>
        <FontPicker
          kind="mono"
          value={t.fontFamily}
          onChange={(id) => update({ ...t, fontFamily: id })}
        />
      </div>
    </div>
  );
}

function CopyButton({ value, label }: { value: string; label: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      className="icon-btn"
      aria-label={`Copy ${label}`}
      title={`Copy ${label}`}
      onClick={async () => {
        if (await copyText(value)) {
          setDone(true);
          setTimeout(() => setDone(false), 1500);
        }
      }}
    >
      {done ? <CheckIcon size={13} /> : <CopyIcon size={13} />}
    </button>
  );
}

const SCOPE_LABELS: Record<NotifyScope, string> = {
  all: "every workspace",
  selected: "only the ones I pick",
  none: "nothing",
};

function NotifySettings({ isTauri }: { isTauri: boolean }) {
  const { state } = useStore();
  const [prefs, setPrefs] = useState(getNotifyPrefs);
  const [webState, setWebState] = useState(webNotifyState);
  const [testState, setTestState] = useState<"idle" | "sending" | "sent" | "failed">("idle");

  function update(next: typeof prefs) {
    setPrefs(next);
    setNotifyPrefs(next);
  }

  // In "all" the list mutes; in "selected" it subscribes. The checkbox always
  // reads as "notify me about this one", so it inverts with the scope.
  const listed = (id: string) => prefs.workspaces.includes(id);

  return (
    <div className="settings-block">
      <div className="settings-label">notifications — done / awaiting input</div>
      <div className="settings-row">
        <span className="settings-value">alerts</span>
        <button
          type="button"
          className={`settings-toggle ${prefs.enabled ? "on" : ""}`}
          onClick={() => update({ ...prefs, enabled: !prefs.enabled })}
        >
          {prefs.enabled ? "on" : "off"}
        </button>
      </div>
      <div className="settings-row">
        <span className="settings-value">sound</span>
        <button
          type="button"
          className={`settings-toggle ${prefs.sound ? "on" : ""}`}
          onClick={() => {
            const next = { ...prefs, sound: !prefs.sound };
            update(next);
            if (next.sound) chime();
          }}
        >
          {prefs.sound ? "on" : "off"}
        </button>
      </div>
      <div className="settings-row">
        <span className="settings-value">message previews</span>
        <button
          type="button"
          className={`settings-toggle ${prefs.previews ? "on" : ""}`}
          onClick={() => update({ ...prefs, previews: !prefs.previews })}
          title="Show response, question, approval, and error text in notifications"
        >
          {prefs.previews ? "detailed" : "status only"}
        </button>
      </div>
      <div className="settings-value dim">
        detailed previews may be visible on your lock screen
      </div>
      <div className="settings-row">
        <span className="settings-value">notify me about</span>
        <div className="settings-seg">
          {(Object.keys(SCOPE_LABELS) as NotifyScope[]).map((scope) => (
            <button
              key={scope}
              type="button"
              className={`settings-toggle ${prefs.scope === scope ? "on" : ""}`}
              onClick={() => update({ ...prefs, scope })}
            >
              {SCOPE_LABELS[scope]}
            </button>
          ))}
        </div>
      </div>
      {prefs.scope !== "none" && (
        <div className="settings-ws-list">
          <div className="settings-value dim">
            {prefs.scope === "selected"
              ? "picked workspaces alert this device; everything else stays silent"
              : "untick a workspace to mute it on this device"}
          </div>
          {state.workspaces.length === 0 && (
            <div className="settings-value dim">no workspaces yet</div>
          )}
          {state.workspaces.map((ws) => (
            <label key={ws.id} className="settings-ws-row">
              <input
                type="checkbox"
                checked={isWorkspaceSubscribed(prefs, ws.id)}
                onChange={() =>
                  update({
                    ...prefs,
                    workspaces: listed(ws.id)
                      ? prefs.workspaces.filter((id) => id !== ws.id)
                      : [...prefs.workspaces, ws.id],
                  })
                }
              />
              <span className="settings-value">{ws.name}</span>
            </label>
          ))}
        </div>
      )}
      <div className="settings-value dim settings-ws-note">
        this choice lives on this device only — other browsers and phones
        signed into the same Threadknot keep their own.
      </div>
      {isTauri && (
        <div className="settings-row settings-notify-test">
          <span className={`settings-value ${testState === "failed" ? "notify-failed" : "dim"}`}>
            {testState === "sending" && "contacting desktop…"}
            {testState === "sent" && "accepted by desktop"}
            {testState === "failed" && "desktop rejected it"}
          </span>
          <button
            type="button"
            className="settings-toggle"
            disabled={testState === "sending"}
            onClick={() => {
              setTestState("sending");
              void testNativeNotification()
                .then(() => setTestState("sent"))
                .catch(() => setTestState("failed"));
            }}
          >
            send test
          </button>
        </div>
      )}
      {!isTauri && webState === "default" && (
        <button
          type="button"
          className="settings-toggle"
          onClick={() => {
            void requestWebPermission().then(() => setWebState(webNotifyState()));
          }}
        >
          enable system notifications
        </button>
      )}
      {!isTauri && webState === "unsupported" && (
        <div className="settings-value dim">
          system notifications need HTTPS — in-app alerts still work
        </div>
      )}
    </div>
  );
}

/** Kebab menu on a machine card. Same portaled-menu pattern as the sidebar's
 *  thread kebab (anchored under the button, outside-click / Escape close),
 *  except Escape is caught in the capture phase so the settings screen
 *  underneath stays open. */
function MachineCardMenu({
  label,
  items,
}: {
  label: string;
  items: { label: string; icon?: ReactNode; danger?: boolean; onSelect: () => void }[];
}) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  // Anchor the portaled menu under the kebab; both render unzoomed, so the
  // button's viewport rect maps 1:1 onto the menu's fixed coordinates.
  useLayoutEffect(() => {
    if (!open) return;
    function place() {
      const b = btnRef.current;
      if (!b) return;
      const r = b.getBoundingClientRect();
      const width = 172;
      setPos({ top: r.bottom + 4, left: Math.max(8, r.right - width) });
    }
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [open]);

  // Close on outside-click / Escape (Escape closes only the menu).
  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      const t = e.target as Node;
      if (btnRef.current?.contains(t)) return;
      if (menuRef.current?.contains(t)) return;
      setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      setOpen(false);
    }
    document.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [open]);

  return (
    <>
      <button
        ref={btnRef}
        type="button"
        className={`icon-btn machine-kebab${open ? " on" : ""}`}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        title={label}
        onClick={() => setOpen((v) => !v)}
      >
        <MoreIcon size={15} />
      </button>
      {open &&
        pos &&
        createPortal(
          <div ref={menuRef} className="thread-menu" role="menu" style={pos}>
            {items.map((it) => (
              <button
                key={it.label}
                type="button"
                role="menuitem"
                className={`thread-menu-item${it.danger ? " danger" : ""}`}
                onClick={() => {
                  setOpen(false);
                  it.onSelect();
                }}
              >
                {it.icon}
                <span>{it.label}</span>
              </button>
            ))}
          </div>,
          document.body,
        )}
    </>
  );
}

/** "192.168.0.4:42800" from this machine's LAN URL (the closest thing to a
 *  hostname the hello payload carries). */
function hostFromLanUrl(lanUrl: string | undefined): string | null {
  if (!lanUrl) return null;
  try {
    const u = new URL(lanUrl);
    return u.port ? `${u.hostname}:${u.port}` : u.hostname;
  } catch {
    return null;
  }
}

/** Phones paired via the Threadknot mobile app, with one-click revocation. */
/** Peer Threadknot machines: this machine's mesh name, paired peers with live
 *  presence, LAN-discovered Threadknots, and one-paste pairing (URL + token). */
function MachinesSettings() {
  const { state, actions } = useStore();
  const [adding, setAdding] = useState(false);
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [renaming, setRenaming] = useState(false);
  const [name, setName] = useState("");
  /** Which machine's Customize Profile popup is open: "local" or a peer id. */
  const [customizing, setCustomizing] = useState<string | null>(null);
  /** For a peer popup: "profile" edits its REAL profile everywhere (routed);
   *  "override" sets a local-only look on this device. Ignored for "local". */
  const [customizeMode, setCustomizeMode] = useState<"profile" | "override">("profile");
  /** Peer machineId with the type-to-delete confirmation open. */
  const [removing, setRemoving] = useState<string | null>(null);
  /** One-click pair state per discovered machineId. */
  const [pairState, setPairState] = useState<Record<string, "busy" | "done">>({});

  const hello = state.hello;
  const localHost = hostFromLanUrl(hello?.lanUrl);

  async function pair() {
    if (!url.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      await actions.addPeer(url.trim(), token.trim() || undefined);
      setUrl("");
      setToken("");
      setAdding(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  function commitRename() {
    const next = name.trim();
    setRenaming(false);
    if (next && next !== hello?.friendlyName) {
      void actions.renameDevice(next).catch(() => undefined);
    }
  }

  function clearPairState(machineId: string) {
    setPairState((m) => {
      const rest = { ...m };
      delete rest[machineId];
      return rest;
    });
  }

  /** Pair a LAN-discovered machine in place: busy → brief green "paired".
   *  When the peer wants a token, fall back to the pair-by-url form. */
  async function quickPair(d: DiscoveredPeer) {
    if (pairState[d.machineId]) return;
    const addr = `${d.addresses[0]}:${d.port}`;
    setPairState((m) => ({ ...m, [d.machineId]: "busy" }));
    try {
      await actions.addPeer(addr);
      setPairState((m) => ({ ...m, [d.machineId]: "done" }));
      window.setTimeout(() => clearPairState(d.machineId), 1600);
    } catch (e) {
      clearPairState(d.machineId);
      setAdding(true);
      setUrl(addr);
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  const localLook = machineLook(state, undefined);

  const pairedIds = new Set(state.peers.map((p) => p.machineId));
  // While the green "paired" flash runs, keep the discovered card on screen
  // (and hold the freshly-added peer card back) so the transition reads.
  const flashing = new Set(
    Object.keys(pairState).filter((id) => pairState[id] === "done"),
  );
  const peers = state.peers.filter((p) => !flashing.has(p.machineId));
  const discovered = state.discovered.filter(
    (d) => !pairedIds.has(d.machineId) || flashing.has(d.machineId),
  );
  const customizingPeer =
    customizing && customizing !== "local"
      ? state.peers.find((p) => p.machineId === customizing)
      : undefined;
  const removingPeer = removing
    ? state.peers.find((p) => p.machineId === removing)
    : undefined;

  return (
    <div className="settings-block">
      <div className="settings-row">
        <span className="settings-label">machines</span>
        <button
          type="button"
          className="settings-toggle"
          title="Pair a machine by pasting its LAN URL + token"
          onClick={() => {
            setAdding((v) => !v);
            setError(null);
          }}
        >
          {adding ? "cancel" : "pair by url"}
        </button>
      </div>

      <div className="machine-cards">
        <div className="machine-card">
          <MachineAvatar {...localLook} size={48} className="machine-card-avatar" />
          <div className="machine-card-main">
            {renaming ? (
              <input
                className="peer-rename-input"
                value={name}
                autoFocus
                aria-label="Rename this machine"
                onChange={(e) => setName(e.target.value)}
                onBlur={commitRename}
                onKeyDown={(e) => {
                  if (e.key === "Enter") commitRename();
                  else if (e.key === "Escape") setRenaming(false);
                }}
              />
            ) : (
              <div className="machine-card-name">
                <span className="machine-card-title">
                  {hello?.friendlyName ?? "this machine"}
                </span>
                <span className="machine-badge">this machine</span>
              </div>
            )}
            <div className="machine-card-sub">{localHost ?? "not connected"}</div>
            <span className="machine-pill online">
              <span className="pill-dot" />
              this machine
            </span>
          </div>
          <div className="machine-card-actions">
            <button
              type="button"
              className="machine-cta"
              title="Avatar + accent color other machines see for this one"
              onClick={() => setCustomizing("local")}
            >
              Customize Profile
            </button>
            <MachineCardMenu
              label="This machine's actions"
              items={[
                {
                  label: "Rename",
                  icon: <PencilIcon size={13} />,
                  onSelect: () => {
                    setName(hello?.friendlyName ?? "");
                    setRenaming(true);
                  },
                },
              ]}
            />
          </div>
        </div>

        {peers.map((p) => {
          const addr = (p.lastGoodAddress ?? p.addresses[0] ?? "?") + ":" + p.port;
          return (
            <div key={p.machineId} className="machine-card">
              <MachineAvatar
                {...machineLook(state, p.machineId)}
                size={48}
                className="machine-card-avatar"
              />
              <div className="machine-card-main">
                <div className="machine-card-name">
                  <span className="machine-card-title">{p.name}</span>
                </div>
                <div className="machine-card-sub">{addr}</div>
                {/* A pair made before the encrypted mesh is a different problem
                    from a machine being asleep: it will never connect, and only
                    updating that machine and re-pairing fixes it. Labelling it
                    "offline" would send someone hunting a network fault. */}
                <span
                  className={`machine-pill${
                    p.needsUpgrade ? " stale" : p.online ? " online" : ""
                  }`}
                  title={
                    p.needsUpgrade
                      ? "This pair predates encrypted mesh connections. Update Threadknot on that machine, then pair the two again."
                      : p.online
                        ? undefined
                        : p.lastSeenAt
                          ? `last seen ${timeAgo(p.lastSeenAt)}`
                          : undefined
                  }
                >
                  <span className="pill-dot" />
                  {p.needsUpgrade
                    ? "update needed"
                    : p.online
                      ? "connected"
                      : "offline"}
                </span>
              </div>
              <div className="machine-card-actions">
                <button
                  type="button"
                  className="machine-cta"
                  title="Edit this machine's real profile (applies on every machine)"
                  onClick={() => {
                    setCustomizeMode("profile");
                    setCustomizing(p.machineId);
                  }}
                >
                  Set profile
                </button>
                <MachineCardMenu
                  label={`Actions for ${p.name}`}
                  items={[
                    {
                      label: "Override locally (this machine only)",
                      icon: <PencilIcon size={13} />,
                      onSelect: () => {
                        setCustomizeMode("override");
                        setCustomizing(p.machineId);
                      },
                    },
                    {
                      label: "Remove…",
                      icon: <TrashIcon size={13} />,
                      danger: true,
                      onSelect: () => setRemoving(p.machineId),
                    },
                  ]}
                />
              </div>
            </div>
          );
        })}

        {discovered.map((d) => {
          const st = pairState[d.machineId];
          return (
            <div key={d.machineId} className="machine-card discovered">
              <MachineAvatar
                name={d.name || "Threadknot"}
                size={48}
                className="machine-card-avatar"
              />
              <div className="machine-card-main">
                <div className="machine-card-name">
                  <span className="machine-card-title">{d.name || "Threadknot"}</span>
                </div>
                <div className="machine-card-sub">
                  {d.addresses[0]}:{d.port}
                </div>
                <span className="machine-pill">
                  <span className="pill-dot" />
                  found on LAN
                </span>
              </div>
              <div className="machine-card-actions">
                <button
                  type="button"
                  className={`machine-cta${st === "done" ? " paired" : ""}`}
                  disabled={st === "busy" || st === "done"}
                  title="Pair with this machine"
                  onClick={() => void quickPair(d)}
                >
                  {st === "busy" ? "pairing…" : st === "done" ? "paired" : "pair"}
                </button>
              </div>
            </div>
          );
        })}
      </div>

      {state.peers.length === 0 && state.discovered.length === 0 && !adding && (
        <div className="settings-value dim">
          none paired — open Threadknot's Settings on the other machine and paste
          its LAN URL here
        </div>
      )}

      {adding && (
        <div className="hermes-add">
          <input
            type="text"
            placeholder="http://192.168.0.99:42800/?token=…"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            autoFocus
          />
          <input
            type="password"
            placeholder="token (only if not in the URL)"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void pair();
            }}
          />
          {error && <div className="settings-value notify-failed">{error}</div>}
          <button
            type="button"
            className="settings-toggle"
            disabled={busy || !url.trim()}
            onClick={() => void pair()}
          >
            {busy ? "pairing…" : "pair machines"}
          </button>
        </div>
      )}

      {customizing === "local" && (
        <CustomizeProfileModal
          name={hello?.friendlyName ?? "this machine"}
          image={hello?.avatar}
          color={hello?.color}
          onSave={(patch) => actions.setDeviceAppearance(patch)}
          onClose={() => setCustomizing(null)}
        />
      )}
      {customizingPeer && (
        <CustomizeProfileModal
          name={customizingPeer.name}
          subtitle={
            customizeMode === "profile"
              ? "Editing this machine's real profile (applies on every machine)."
              : "Local override: changes how this machine looks on this device only."
          }
          image={
            customizeMode === "profile"
              ? customizingPeer.avatar
              : (customizingPeer.avatarOverride ?? customizingPeer.avatar)
          }
          color={
            customizeMode === "profile"
              ? customizingPeer.color
              : (customizingPeer.colorOverride ?? customizingPeer.color)
          }
          onSave={(patch) =>
            customizeMode === "profile"
              ? actions.setPeerProfile(customizingPeer.machineId, patch)
              : actions.setPeerAppearance(customizingPeer.machineId, patch)
          }
          onClose={() => setCustomizing(null)}
        />
      )}
      {removingPeer && (
        <ConfirmRemoveMachineModal
          name={removingPeer.name}
          onConfirm={() => actions.removePeer(removingPeer.machineId)}
          onClose={() => setRemoving(null)}
        />
      )}
    </div>
  );
}

/** Bytes as something a person can read. Deliberately coarse: nobody needs a
 *  fair-use figure to the byte, and precision here reads as importance. */
function bytes(n: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = n;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 && unit > 0 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

/** The hosted relay connector: paste a token from the console, and this machine
 *  becomes reachable from anywhere.
 *
 *  This is the paid path, so the panel is explicit about the trade rather than
 *  quiet about it: the relay terminates TLS, which means its operator *can*
 *  inspect traffic. The build plan forbids ever implying otherwise, and a
 *  settings panel is exactly where someone decides whether they mind.
 */
function ConnectorSettings() {
  const { actions } = useStore();
  const [status, setStatus] = useState<ConnectorStatus | null>(null);
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    void actions
      .getConnectorStatus()
      .then(setStatus)
      .catch(() => undefined);
  }, [actions]);

  useEffect(() => {
    load();
    // The connector pushes a `connector` state pulse on every change, but a
    // slow poll also covers the case where this panel is open while a reconnect
    // is cycling — the interesting states here are transient by nature.
    const timer = window.setInterval(load, 5000);
    return () => window.clearInterval(timer);
  }, [load]);

  const run = (work: Promise<ConnectorStatus>) => {
    setBusy(true);
    setError(null);
    void work
      .then((s) => {
        setStatus(s);
        setToken("");
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setBusy(false));
  };

  // Owner-only on the server, so a device simply never gets a value back.
  if (!status) return null;

  const enrolled = status.state !== "off" && status.state !== "unenrolled";
  const quota =
    status.monthBytes !== undefined && status.monthQuotaBytes
      ? Math.min(100, Math.round((status.monthBytes / status.monthQuotaBytes) * 100))
      : null;

  return (
    <div className="settings-block">
      <div className="settings-row">
        <span className="settings-label">reachable from anywhere</span>
        <span
          className={`machine-pill${
            status.state === "online"
              ? " online"
              : status.state === "error"
                ? " stale"
                : ""
          }`}
        >
          <span className="pill-dot" />
          {status.state === "unenrolled" ? "not set up" : status.state}
        </span>
      </div>

      {status.hostname && (
        <div className="settings-row">
          <code className="settings-value">{status.publicOrigin}</code>
          <button
            type="button"
            className="settings-toggle"
            onClick={() => void navigator.clipboard?.writeText(status.publicOrigin)}
          >
            copy
          </button>
        </div>
      )}

      {status.state === "off" || status.state === "unenrolled" ? (
        status.approval ? (
          /* A request is out. The connector polls for the answer itself, so this
             is only a display — closing the panel does not abandon it. */
          <ApprovalPanel
            approval={status.approval}
            busy={busy}
            onCancel={() => run(actions.cancelConnectorApproval())}
            onRetry={() => {
              setError(null);
              void actions
                .beginConnectorApproval()
                .then(load)
                .catch((e: unknown) =>
                  setError(e instanceof Error ? e.message : String(e)),
                );
            }}
          />
        ) : (
          <>
            <div className="settings-row">
              <button
                type="button"
                className="settings-toggle primary"
                disabled={busy}
                onClick={() => {
                  setBusy(true);
                  setError(null);
                  void actions
                    .beginConnectorApproval()
                    .then(load)
                    .catch((e: unknown) =>
                      setError(e instanceof Error ? e.message : String(e)),
                    )
                    .finally(() => setBusy(false));
                }}
              >
                {busy ? "starting…" : "connect this machine"}
              </button>
              <span className="settings-value dim">
                opens app.threadknot.ai to sign in and approve. nothing to copy.
              </span>
            </div>

            {/* Demoted, not removed: a headless or scripted install still has a
                token, and so does anyone who already minted one. Behind a
                disclosure because offering both as equals is what made this the
                confusing step in the first place. */}
            <details className="settings-details">
              <summary className="settings-value dim">
                or paste a token from the console
              </summary>
              <div className="settings-row" style={{ marginTop: 6 }}>
                <input
                  className="settings-input"
                  placeholder="paste the token from app.threadknot.ai"
                  value={token}
                  spellCheck={false}
                  autoComplete="off"
                  onChange={(e) => setToken(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && token.trim()) {
                      run(actions.enrollConnector(token.trim()));
                    }
                  }}
                />
                <button
                  type="button"
                  className="settings-toggle"
                  disabled={busy || !token.trim()}
                  onClick={() => run(actions.enrollConnector(token.trim()))}
                >
                  set up
                </button>
              </div>
              <div className="settings-value dim">
                single-use, expires in 15 minutes. this machine keeps its own key —
                the token only proves you authorised it.
              </div>
            </details>
          </>
        )
      ) : (
        <div className="settings-row">
          <button
            type="button"
            className={`settings-toggle${status.state === "online" ? " primary" : ""}`}
            disabled={busy}
            onClick={() => run(actions.setConnectorEnabled(false))}
          >
            turn off
          </button>
          <span className="settings-value dim">
            {status.liveStreams > 0
              ? `${status.liveStreams} live connection${status.liveStreams === 1 ? "" : "s"}`
              : "idle"}
            {status.bytesIn + status.bytesOut > 0 &&
              ` · ${bytes(status.bytesIn + status.bytesOut)} this session`}
          </span>
        </div>
      )}

      {enrolled && !status.acceptingNewSessions && (
        <div className="settings-value error">
          {/* Verbatim from the control plane: it knows which limit was hit and
              this build does not. Paraphrasing would drift. */}
          {status.holdReason ??
            "new connections are on hold. anything already open keeps working."}
        </div>
      )}

      {/* The warning, not the obituary. `acceptingNewSessions` is still true
          here: this is the only advance notice a customer gets, because the
          console is a place they have no reason to visit while everything works. */}
      {enrolled &&
        status.acceptingNewSessions &&
        status.trialDaysLeft !== undefined &&
        status.trialDaysLeft <= 7 && (
          <div className={`settings-value${status.trialDaysLeft <= 3 ? " error" : " dim"}`}>
            {status.trialDaysLeft > 0
              ? `${status.trialDaysLeft} day${status.trialDaysLeft === 1 ? "" : "s"} left in the trial. after that this machine finishes the sessions it has and stops taking new ones — nothing is cut off mid-session. subscribe at app.threadknot.ai/billing.`
              : "the trial ends today. subscribe at app.threadknot.ai/billing to keep opening new sessions."}
          </div>
        )}

      {quota !== null && quota >= 80 && (
        <div className="settings-value dim">
          {bytes(status.monthBytes ?? 0)} of {bytes(status.monthQuotaBytes ?? 0)} fair
          use this month ({quota}%). past the limit, transfer is slowed — never
          billed, never cut off.
        </div>
      )}

      {status.lastError && status.state !== "online" && (
        <div className="settings-value dim">{status.lastError}</div>
      )}

      <div className="settings-value dim">
        {enrolled
          ? "the relay decrypts traffic to route it, so its operator can technically see what passes through — source, terminal output, browser sessions. it stores none of it."
          : "off — nothing outside this network can reach this machine."}
      </div>
      {error && <div className="settings-value error">{error}</div>}
    </div>
  );
}

/** A connection request waiting for someone to approve it in the console.
 *
 *  The link is the whole point, and it is deliberately the largest thing here:
 *  the person is sitting at this machine, so the fastest route is opening the
 *  page on this machine. The code underneath is a fallback for a box with no
 *  browser — it is not the intended path and is not presented as one.
 *
 *  Nothing on screen is a credential. The secret that collects the enrollment
 *  never leaves the Rust side, so this panel is safe on a shared display.
 */
function ApprovalPanel({
  approval,
  busy,
  onCancel,
  onRetry,
}: {
  approval: NonNullable<ConnectorStatus["approval"]>;
  busy: boolean;
  onCancel: () => void;
  onRetry: () => void;
}) {
  const [remaining, setRemaining] = useState(() => secondsLeft(approval.expiresAt));

  useEffect(() => {
    setRemaining(secondsLeft(approval.expiresAt));
    const timer = window.setInterval(
      () => setRemaining(secondsLeft(approval.expiresAt)),
      1000,
    );
    return () => window.clearInterval(timer);
  }, [approval.expiresAt]);

  // Expiry is enforced by the control plane; this only stops the panel claiming
  // a window that has already closed.
  const dead = approval.state !== "waiting" || remaining <= 0;

  if (dead) {
    return (
      <>
        <div className="settings-value error">
          {approval.state === "denied"
            ? "that request was declined in the console."
            : "that request expired before it was approved."}
        </div>
        <div className="settings-row">
          <button
            type="button"
            className="settings-toggle primary"
            disabled={busy}
            onClick={onRetry}
          >
            try again
          </button>
          <button type="button" className="settings-toggle" disabled={busy} onClick={onCancel}>
            cancel
          </button>
        </div>
      </>
    );
  }

  return (
    <>
      <div className="settings-row">
        <a
          className="settings-toggle primary"
          href={approval.verificationUriComplete}
          target="_blank"
          rel="noreferrer noopener"
        >
          open the approval page
        </a>
        <span className="settings-value dim">
          sign in and press approve — {remaining}s left
        </span>
      </div>
      <div className="settings-value dim">
        waiting for approval. this machine is watching for it, so you can close
        settings. if the page did not open, go to {approval.verificationUri} and
        enter <code className="settings-value">{approval.userCode}</code>.
      </div>
      <div className="settings-row">
        <button type="button" className="settings-toggle" disabled={busy} onClick={onCancel}>
          cancel
        </button>
      </div>
    </>
  );
}

/** Whole seconds until an ISO instant, floored at zero. */
function secondsLeft(iso: string): number {
  const at = Date.parse(iso);
  if (Number.isNaN(at)) return 0;
  return Math.max(0, Math.round((at - Date.now()) / 1000));
}

/** Remote access for this machine: the public address it has been given, and
 *  whether the strict ingress answers at all.
 *
 *  Deliberately blunt about what it is. Everything else in Threadknot is
 *  reachable only from this network; this is the one switch that puts a
 *  workstation behind a public hostname, so the copy says so rather than
 *  calling it "sharing".
 */
function RemoteAccessSettings() {
  const { actions } = useStore();
  const [remote, setRemote] = useState<RemoteAccess | null>(null);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void actions
      .getRemoteAccess()
      .then((r) => {
        if (cancelled) return;
        setRemote(r);
        setDraft(r.origin ?? "");
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [actions]);

  const apply = (patch: { enabled?: boolean; origin?: string | null }) => {
    setBusy(true);
    setError(null);
    void actions
      .setRemoteAccess(patch)
      .then((r) => {
        setRemote(r);
        setDraft(r.origin ?? "");
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setBusy(false));
  };

  // Only rendered for the desktop owner: the RPC is master-only, so a device
  // simply never gets a value back.
  if (!remote) return null;

  return (
    <div className="settings-block">
      <div className="settings-row">
        <span className="settings-label">or your own tunnel</span>
        <button
          type="button"
          className={`settings-toggle${remote.enabled ? " primary" : ""}`}
          disabled={busy || (!remote.enabled && !remote.origin)}
          title={
            remote.origin
              ? "Answer requests forwarded from the public address"
              : "Set a public address first"
          }
          onClick={() => apply({ enabled: !remote.enabled })}
        >
          {remote.enabled ? "on" : "off"}
        </button>
      </div>
      <div className="settings-row">
        <input
          className="settings-input"
          placeholder="https://your-machine.remote.threadknot.app"
          value={draft}
          spellCheck={false}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") apply({ origin: draft.trim() || null });
          }}
        />
        <button
          type="button"
          className="settings-toggle"
          disabled={busy || draft.trim() === (remote.origin ?? "")}
          onClick={() => apply({ origin: draft.trim() || null })}
        >
          save
        </button>
      </div>
      <div className="settings-value dim">
        {remote.enabled
          ? `this machine answers on its public address. turning this off signs every remote browser out immediately${remote.browserSessions ? ` (${remote.browserSessions} open)` : ""}.`
          : "off — nothing outside this network can reach this machine. the connector talks to 127.0.0.1:" +
            remote.loopbackPort +
            ", never to the network."}
      </div>
      {error && <div className="settings-value error">{error}</div>}
    </div>
  );
}

function MobileDevices() {
  const { actions } = useStore();
  const [devices, setDevices] = useState<
    import("../lib/protocol").MobileDeviceInfo[] | null
  >(null);
  const [pairing, setPairing] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void actions
      .listMobileDevices()
      .then((d) => {
        if (!cancelled) setDevices(d);
      })
      .catch(() => {
        if (!cancelled) setDevices([]);
      });
    return () => {
      cancelled = true;
    };
  }, [actions]);

  return (
    <div className="settings-block">
      <div className="settings-row">
        <span className="settings-label">paired phones</span>
        <button
          type="button"
          className="settings-toggle primary"
          title="Show a QR code for the Threadknot mobile app to scan"
          onClick={() => setPairing(true)}
        >
          pair a phone
        </button>
      </div>
      {(!devices || devices.length === 0) && (
        <div className="settings-value dim">
          none yet — tap “pair a phone” and scan the QR from the Threadknot app,
          or paste the LAN URL above into it
        </div>
      )}
      {pairing && (
        <PairPhoneModal
          knownDeviceIds={(devices ?? []).map((d) => d.id)}
          onPaired={(d) =>
            setDevices((prev) =>
              prev?.some((x) => x.id === d.id) ? prev : [...(prev ?? []), d],
            )
          }
          onClose={() => setPairing(false)}
        />
      )}
      {(devices ?? []).map((d) => (
        <PairedPhoneRow
          key={d.id}
          device={d}
          onChanged={(next) =>
            setDevices((prev) => (prev ?? []).map((x) => (x.id === next.id ? next : x)))
          }
          onRevoked={() =>
            setDevices((prev) => (prev ?? []).filter((x) => x.id !== d.id))
          }
        />
      ))}
    </div>
  );
}

/** One paired phone: name, revoke, and the grants it holds.
 *
 *  The grants are the whole point of the row. A phone is a remote-control for a
 *  workstation — "paired" is not one thing, it is a set of very different
 *  consequences (read a chat / open a shell / act as your logged-in accounts),
 *  and the owner has to be able to see and change which ones this device got.
 */
function PairedPhoneRow({
  device,
  onChanged,
  onRevoked,
}: {
  device: import("../lib/protocol").MobileDeviceInfo;
  onChanged: (device: import("../lib/protocol").MobileDeviceInfo) => void;
  onRevoked: () => void;
}) {
  const { actions } = useStore();
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const granted = device.capabilities ?? DEFAULT_DEVICE_CAPABILITIES;

  const toggle = (id: DeviceCapability, on: boolean) => {
    const next = on
      ? [...granted, id]
      : granted.filter((c) => c !== id);
    setError(null);
    void actions
      .setMobileDeviceCapabilities(device.id, next)
      .then(onChanged)
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)));
  };

  return (
    <div className="settings-subblock">
      <div className="settings-row">
        <span className="settings-value">
          {device.name}
          <span className="dim">
            {" "}
            · {device.platform}
            {device.expoPushToken ? "" : " · no push"}
          </span>
        </span>
        <span className="settings-row-actions">
          <button
            type="button"
            className="settings-toggle"
            aria-expanded={open}
            title="What this phone is allowed to do"
            onClick={() => setOpen((v) => !v)}
          >
            {granted.length} of {DEVICE_CAPABILITY_LABELS.length} permissions
          </button>
          <button
            type="button"
            className="settings-toggle"
            title="Revoke this phone's access"
            onClick={() => {
              void actions
                .revokeMobileDevice(device.id)
                .then(onRevoked)
                .catch(() => undefined);
            }}
          >
            revoke
          </button>
        </span>
      </div>
      {open && (
        <>
          <CapabilityPicker granted={granted} onToggle={toggle} />
          <div className="settings-value dim">
            taking a permission away also closes whatever this phone has open
            right now
          </div>
          {error && <div className="settings-value error">{error}</div>}
        </>
      )}
    </div>
  );
}

/** The grant checklist, shared by the pairing dialog and the device row. */
export function CapabilityPicker({
  granted,
  onToggle,
}: {
  granted: DeviceCapability[];
  onToggle: (id: DeviceCapability, on: boolean) => void;
}) {
  return (
    <div className="capability-picker">
      {DEVICE_CAPABILITY_LABELS.map((c) => (
        <label key={c.id} className="capability-option">
          <input
            type="checkbox"
            checked={granted.includes(c.id)}
            onChange={(e) => onToggle(c.id, e.target.checked)}
          />
          <span className="capability-label">{c.label}</span>
          <span className="capability-detail dim">{c.detail}</span>
        </label>
      ))}
    </div>
  );
}

/** One registered Hermes gateway: status row + expandable live details
 *  (version, toolsets — including MCP-mounted ones — and skills). */
function HermesAgentRow({
  agent,
  onRemove,
  onImageChange,
}: {
  agent: HermesAgentInfo;
  onRemove: () => void;
  onImageChange: (agent: HermesAgentInfo) => void;
}) {
  const { state, actions } = useStore();
  const [open, setOpen] = useState(false);
  const [details, setDetails] = useState<HermesAgentDetails | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Live presence (the always-fresh source); the on-demand details below add
  // version + toolsets, so they no longer repeat the online/offline line.
  const presence = hermesPresence(state.hermesStatuses[agent.id]);

  useEffect(() => {
    if (!open || details) return;
    let cancelled = false;
    setError(null);
    void actions
      .hermesAgentDetails(agent.id)
      .then((d) => {
        if (!cancelled) setDetails(d);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [open, details, actions, agent.id]);

  const host = agent.baseUrl.replace(/^https?:\/\//, "");
  const photo = agent.avatar ?? agent.image;
  const avatarPreview = useAvatarHoverPreview({ image: photo, name: agent.name });
  return (
    <div className="hermes-agent">
      <div className="settings-row">
        <button
          type="button"
          className={`settings-agent-avatar sidebar-avatar${photo ? " has-image" : ""}`}
          {...avatarPreview.hoverProps}
          title={photo ? "Change image" : "Set image"}
          aria-label={
            photo
              ? `Change ${agent.name}'s profile picture`
              : `Set a profile picture for ${agent.name}`
          }
          onClick={() => {
            void pickAvatarImage()
              .then((image) =>
                image ? actions.setHermesAgentAvatar(agent.id, image) : undefined,
              )
              .then((updated) => {
                if (updated) onImageChange(updated);
              })
              .catch((e: unknown) =>
                setError(e instanceof Error ? e.message : String(e)),
              );
          }}
        >
          {photo ? <img src={photo} alt="" /> : <AgentMark agent="hermes" size={12} />}
        </button>
        {avatarPreview.portal}
        <button
          type="button"
          className="hermes-agent-head"
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
        >
          <ChevronIcon size={10} open={open} />
          <span className="settings-value">
            <span className={`hermes-presence-inline ${presence.kind}`} title={presence.title} />
            {agent.name}
            <span className="dim"> · {host} · {presence.label}</span>
          </span>
        </button>
        {agent.avatar && (
          <button
            type="button"
            className="settings-toggle"
            title="Remove profile picture"
            onClick={() => {
              void actions
                .setHermesAgentAvatar(agent.id, null)
                .then(onImageChange)
                .catch((e: unknown) =>
                  setError(e instanceof Error ? e.message : String(e)),
                );
            }}
          >
            clear image
          </button>
        )}
        <button
          type="button"
          className="settings-toggle"
          title="Remove this Hermes agent"
          onClick={onRemove}
        >
          remove
        </button>
      </div>
      {open && (
        <div className="hermes-agent-details">
          {!details && !error && <div className="settings-value dim">contacting gateway…</div>}
          {error && <div className="settings-value notify-failed">unreachable: {error}</div>}
          {details && (
            <>
              {/* Online/offline lives on the header row (live status); here we
                  add only what details carry beyond it: version, and an
                  unhealthy note when the gateway itself reports trouble. */}
              {details.health.version && (
                <div className="settings-value dim">hermes {details.health.version}</div>
              )}
              {!details.health.ok && (
                <div className="settings-value notify-failed">gateway reports unhealthy</div>
              )}
              <div className="settings-label">tools & mcp toolsets</div>
              <div className="hermes-list">
                {details.toolsets.filter((t) => t.enabled).map((t) => (
                  <div key={t.name} className="hermes-item" title={t.tools.join(", ")}>
                    <span>{t.label}</span>
                    <span className="dim">{t.tools.length}</span>
                  </div>
                ))}
                {details.toolsets.filter((t) => t.enabled).length === 0 && (
                  <div className="settings-value dim">no toolsets enabled</div>
                )}
              </div>
              <div className="settings-label">skills · {details.skills.length}</div>
              <div className="hermes-list">
                {details.skills.map((s) => (
                  <div key={s.name} className="hermes-item" title={s.description}>
                    <span>{s.name}</span>
                    {s.category && <span className="dim">{s.category}</span>}
                  </div>
                ))}
              </div>
            </>
          )}
        </div>
      )}
    </div>
  );
}

function ArchiveRow({
  a,
  machineId,
  onRestored,
}: {
  a: ArchiveHeader;
  /** Owning machine (undefined = this machine). Routes restore/delete. */
  machineId?: string;
  /** Called after a successful restore so the caller can close Settings — the
   *  restore action itself has already navigated to the restored thread. */
  onRestored: () => void;
}) {
  const { actions } = useStore();
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    if (!confirm) return;
    const t = setTimeout(() => setConfirm(false), 2600);
    return () => clearTimeout(t);
  }, [confirm]);
  return (
    <div className="archive-row">
      <div className="archive-main">
        <span className="archive-title">{a.title || "Untitled thread"}</span>
        <span className="archive-sub">
          <span className="archive-proj">{a.projectName}</span>
          <span className="archive-dot">·</span>
          <span>{timeAgo(a.archivedAt)}</span>
          <span className="archive-dot">·</span>
          <span className="archive-count">{a.eventCount} events</span>
          {a.terminalCount > 0 && (
            <>
              <span className="archive-dot">·</span>
              <span className="archive-count">{a.terminalCount} term</span>
            </>
          )}
        </span>
        {err && <span className="archive-err">{err}</span>}
      </div>
      <div className="archive-actions">
        <button
          type="button"
          className="archive-btn"
          disabled={busy}
          onClick={async () => {
            setErr(null);
            setBusy(true);
            try {
              // restoreArchive navigates to the restored thread internally; on
              // success we close Settings so the user lands in it.
              await actions.restoreArchive(a.id, machineId);
              onRestored();
            } catch (e) {
              setErr(e instanceof Error ? e.message : String(e));
              setBusy(false);
            }
          }}
        >
          {busy ? "restoring…" : "restore"}
        </button>
        <button
          type="button"
          className={`archive-btn danger${confirm ? " armed" : ""}`}
          disabled={busy}
          aria-label={confirm ? "Confirm delete archive" : "Delete archive"}
          title={confirm ? "Click again to confirm" : "Delete archive"}
          onClick={async () => {
            if (!confirm) {
              setConfirm(true);
              return;
            }
            setErr(null);
            setBusy(true);
            try {
              await actions.deleteArchive(a.id, machineId);
            } catch (e) {
              setErr(e instanceof Error ? e.message : String(e));
            } finally {
              setBusy(false);
              setConfirm(false);
            }
          }}
        >
          {confirm ? "sure?" : "delete"}
        </button>
      </div>
    </div>
  );
}

/** Remote Hermes gateways: list, add (URL + key, probed before save), remove. */
function HermesAgents() {
  const { actions } = useStore();
  const [agents, setAgents] = useState<HermesAgentInfo[] | null>(null);
  const [adding, setAdding] = useState(false);
  const [, setEnabledTick] = useState(0);
  const enabled = getHermesEnabled();
  const [url, setUrl] = useState("");
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void actions
      .listHermesAgents()
      .then((a) => {
        if (!cancelled) setAgents(a);
      })
      .catch(() => {
        if (!cancelled) setAgents([]);
      });
    return () => {
      cancelled = true;
    };
  }, [actions]);

  async function add() {
    if (!url.trim() || !key.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      const agent = await actions.addHermesAgent(url.trim(), key.trim());
      setAgents([...(agents ?? []), agent]);
      setUrl("");
      setKey("");
      setAdding(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="settings-block">
      <div className="settings-row">
        <span className="settings-label">hermes agents</span>
        {/* The master switch: registered gateways stay dark everywhere (agent
            pickers, sidebar, scheduled runs) until this is deliberately on.
            Off by default on every machine. */}
        <span className="settings-seg">
          <button
            type="button"
            className={`settings-toggle${enabled ? " on" : ""}`}
            onClick={() => {
              setHermesEnabled(!enabled);
              setEnabledTick((n) => n + 1);
            }}
          >
            {enabled ? "enabled" : "off"}
          </button>
        </span>
        <button
          type="button"
          className="settings-toggle"
          onClick={() => {
            setAdding((v) => !v);
            setError(null);
          }}
        >
          {adding ? "cancel" : "add"}
        </button>
      </div>
      {!enabled && (
        <div className="settings-hint">
          Off: Hermes agents stay out of the agent pickers, the sidebar, and
          scheduled runs on this machine until you enable them.
        </div>
      )}
      {(agents ?? []).map((a) => (
        <HermesAgentRow
          key={a.id}
          agent={a}
          onImageChange={(updated) =>
            setAgents((agents ?? []).map((x) => (x.id === updated.id ? updated : x)))
          }
          onRemove={() => {
            void actions
              .removeHermesAgent(a.id)
              .then(() => setAgents((agents ?? []).filter((x) => x.id !== a.id)))
              .catch(() => undefined);
          }}
        />
      ))}
      {agents !== null && agents.length === 0 && !adding && (
        <div className="settings-value dim">
          none yet — add a gateway URL (e.g. http://host:8651/v1) and its API key
        </div>
      )}
      {adding && (
        <div className="hermes-add">
          <input
            type="text"
            placeholder="http://192.168.0.97:8651/v1"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            autoFocus
          />
          <input
            type="password"
            placeholder="API key"
            value={key}
            onChange={(e) => setKey(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void add();
            }}
          />
          {error && <div className="settings-value notify-failed">{error}</div>}
          <button
            type="button"
            className="settings-toggle"
            disabled={busy || !url.trim() || !key.trim()}
            onClick={() => void add()}
          >
            {busy ? "checking gateway…" : "connect"}
          </button>
        </div>
      )}
    </div>
  );
}

/** Archives, scoped to one machine at a time. First chip is this machine
 *  (default); the rest are paired peers with live presence. Selecting a machine
 *  loads its list on demand (routed for peers) and remembers the outcome so an
 *  offline peer shows a clear message instead of hanging. */
function ArchivesSettings({ onClose }: { onClose: () => void }) {
  const { state, actions } = useStore();
  const [dir, setDir] = useState("");
  const [picking, setPicking] = useState(false);
  /** Selected owner: undefined = this machine, else a peer machineId. */
  const [selected, setSelected] = useState<string | undefined>(undefined);
  /** Per-machine load outcome, keyed by resolved storage key. */
  const [status, setStatus] = useState<
    Record<string, "loading" | "loaded" | "error">
  >({});
  const [errors, setErrors] = useState<Record<string, string>>({});

  const localId = state.hello?.machineId;
  const localName = state.hello?.friendlyName ?? "this machine";
  const isLocal = selected === undefined || selected === localId;
  /** Storage key for a selection: the local id (or "") for this machine. */
  const keyFor = (sel: string | undefined) => (sel === undefined ? localId ?? "" : sel);

  const load = useCallback(
    async (sel: string | undefined) => {
      const key = keyFor(sel);
      setStatus((m) => ({ ...m, [key]: "loading" }));
      setErrors((m) => {
        const rest = { ...m };
        delete rest[key];
        return rest;
      });
      try {
        await actions.refreshArchives(sel);
        setStatus((m) => ({ ...m, [key]: "loaded" }));
      } catch (e) {
        setStatus((m) => ({ ...m, [key]: "error" }));
        setErrors((m) => ({
          ...m,
          [key]: e instanceof Error ? e.message : String(e),
        }));
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [actions, localId],
  );

  // Archive dir is local-only; fetch it once.
  useEffect(() => {
    void actions.getArchiveDir().then(setDir).catch(() => undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Always refresh when a machine is selected (and on mount for the initial
  // machine): a cached peer list can be stale if broadcasts were missed while
  // the peer was disconnected, so it has to reconcile on view. The cached list
  // stays rendered while the refresh runs (see `loading`/`failed` below), and a
  // failed refresh over cached data surfaces inline without clearing it.
  useEffect(() => {
    void load(selected);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected, localId]);

  const key = keyFor(selected);
  const list = state.archives[key];
  const visibleList = list?.filter((a) => isAgentVisible(a.agent));
  const st = status[key];
  const loading = st === "loading" && list === undefined;
  const failed = st === "error" && list === undefined;
  const selectedName = isLocal
    ? localName
    : state.peers.find((p) => p.machineId === selected)?.name ?? "that machine";

  const chip = (sel: string | undefined, name: string, online: boolean) => {
    const count = state.archives[keyFor(sel)]?.filter((a) => isAgentVisible(a.agent)).length;
    const on = sel === undefined ? isLocal : selected === sel;
    return (
      <button
        key={sel ?? "local"}
        type="button"
        className={`archive-chip${on ? " on" : ""}`}
        aria-current={on}
        onClick={() => setSelected(sel)}
      >
        <MachineAvatar {...machineLook(state, sel)} size={18} preview={false} />
        <span className="archive-chip-name">{name}</span>
        <span
          className={`archive-chip-dot${online ? " online" : ""}`}
          title={online ? "online" : "offline"}
        />
        {count !== undefined && <span className="archive-chip-count">{count}</span>}
      </button>
    );
  };

  return (
    <div className="settings-block archives-page">
      <div className="settings-label">archives</div>

      <div className="archive-machines">
        {chip(undefined, localName, true)}
        {state.peers.map((p) => chip(p.machineId, p.name, !!p.online))}
      </div>

      {isLocal ? (
        <div className="settings-row archive-loc">
          <code className="archive-dir" title={dir || undefined}>
            {dir || "—"}
          </code>
          <button type="button" className="settings-toggle" onClick={() => setPicking(true)}>
            change
          </button>
        </div>
      ) : (
        <div className="settings-value dim archive-loc-note">
          storage location is managed on {selectedName}
        </div>
      )}

      <div className="archive-list archive-list-fill">
        {st === "error" && list !== undefined && errors[key] && (
          <div className="archive-err archive-refresh-err">{errors[key]}</div>
        )}
        {loading ? (
          <div className="archive-empty">loading {selectedName}'s archives…</div>
        ) : failed ? (
          <div className="archive-offline">
            <span>
              couldn't reach {selectedName} — the machine looks offline.
            </span>
            {errors[key] && <span className="archive-err">{errors[key]}</span>}
            <button
              type="button"
              className="settings-toggle"
              onClick={() => void load(selected)}
            >
              retry
            </button>
          </div>
        ) : (visibleList ?? []).length === 0 ? (
          <div className="archive-empty">
            no archived sessions on {selectedName}
          </div>
        ) : (
          (visibleList ?? []).map((a) => (
            <ArchiveRow key={a.id} a={a} machineId={selected} onRestored={onClose} />
          ))
        )}
      </div>

      {picking && (
        <DirPicker
          title="Archive storage location"
          confirmLabel="Use this folder"
          onClose={() => setPicking(false)}
          onPick={(path) => {
            void actions.setArchiveDir(path).then(setDir).catch(() => undefined);
          }}
        />
      )}
    </div>
  );
}

/** Connection status + this server's LAN address/token + paired phones. */
function PhoneAccessSettings() {
  const { state } = useStore();
  const hello = state.hello;
  const lanUrl = hello?.lanUrl ?? "";
  const token =
    localStorage.getItem("threadknot.token") ??
    localStorage.getItem("armada.token") ??
    "";
  const hasTokenInUrl = lanUrl.includes("token=");
  const phoneUrl =
    lanUrl && !hasTokenInUrl && token
      ? `${lanUrl}${lanUrl.includes("?") ? "&" : "?"}token=${token}`
      : lanUrl;

  return (
    <>
      <div className="settings-block">
        <div className="settings-label">connection</div>
        <div className="settings-row">
          <span className="settings-value">status</span>
          <span className={`settings-value conn-text conn-${state.conn}`}>{state.conn}</span>
        </div>
        <div className="settings-row">
          <span className="settings-value">server version</span>
          <span className="settings-value">{hello ? `v${hello.version}` : "—"}</span>
        </div>
      </div>

      <div className="settings-block">
        <div className="settings-label">LAN URL — open on your phone (or paste on another machine to pair)</div>
        {phoneUrl ? (
          <div className="settings-copyline">
            <code className="settings-url">{phoneUrl}</code>
            <CopyButton value={phoneUrl} label="LAN URL" />
          </div>
        ) : (
          <div className="settings-value dim">not connected</div>
        )}
      </div>

      {token && !hasTokenInUrl && (
        <div className="settings-block">
          <div className="settings-label">token</div>
          <div className="settings-copyline">
            <code className="settings-url">{token.slice(0, 6)}…{token.slice(-4)}</code>
            <CopyButton value={token} label="token" />
          </div>
        </div>
      )}

      {/* The hosted relay first: it is the path that works from a phone on
          cellular with no setup beyond a pasted token. The manual origin below
          stays for people running their own tunnel, which the threat model
          explicitly keeps as a supported transport. */}
      <ConnectorSettings />
      <RemoteAccessSettings />
      <MobileDevices />
    </>
  );
}

/** Agent availability + registered remote Hermes gateways. */
function AgentsSettings() {
  const { state } = useStore();
  const hello = state.hello;
  return (
    <>
      {hello && (
        <div className="settings-block">
          <div className="settings-label">agents on this machine</div>
          {hello.agents.filter((a) => isAgentVisible(a.id)).map((a) => (
            <div key={a.id} className="settings-row">
              <span className="settings-value settings-agent">
                <AgentMark agent={a.id} size={12} />
                {a.name}
              </span>
              <span className={`settings-value ${a.available ? "ok" : "dim"}`}>
                {a.available ? "ready" : (a.authHint ?? "unavailable")}
              </span>
            </div>
          ))}
        </div>
      )}
      <HermesAgents />
      <ClaudexProfiles />
    </>
  );
}

/** The default a new profile starts from: a local claude-code-proxy bridging
 *  to Codex on a ChatGPT subscription. Everything is editable — this is the
 *  setup most people want, not the only one that works. */
const CLAUDEX_PRESET: ClaudexProfileInput = {
  name: "GPT-5.6 Sol",
  baseUrl: "http://127.0.0.1:18765",
  model: "gpt-5.6-sol",
  smallModel: "gpt-5.6-luna",
  // Left blank the server reads the provider's own catalog, which is both
  // per-account and correct — a hardcoded guess here would be neither.
  efforts: ["low", "medium", "high", "xhigh", "max"],
  defaultEffort: "high",
  sidecar: { command: "claude-code-proxy", args: ["serve"] },
};

/** Claudex profiles: the Claude Code harness on a non-Anthropic model. */
function ClaudexProfiles() {
  const { actions } = useStore();
  const [profiles, setProfiles] = useState<ClaudexProfileInfo[] | null>(null);
  const [editing, setEditing] = useState<ClaudexProfileInfo | "new" | null>(null);

  useEffect(() => {
    let cancelled = false;
    void actions
      .listClaudexProfiles()
      .then((p) => {
        if (!cancelled) setProfiles(p);
      })
      .catch(() => {
        if (!cancelled) setProfiles([]);
      });
    return () => {
      cancelled = true;
    };
  }, [actions]);

  return (
    <div className="settings-block">
      <div className="settings-row">
        <span className="settings-label">claudex profiles</span>
        <button
          type="button"
          className="settings-toggle"
          onClick={() => setEditing((v) => (v === "new" ? null : "new"))}
        >
          {editing === "new" ? "cancel" : "add"}
        </button>
      </div>
      <div className="settings-value dim claudex-note">
        Claude Code's harness driven by another model over an Anthropic-compatible
        bridge. Usage bills to whatever the bridge is signed into — not your Claude
        plan.
      </div>
      {(profiles ?? []).map((p) => (
        <ClaudexProfileRow
          key={p.id}
          profile={p}
          editing={editing !== "new" && editing?.id === p.id}
          onEdit={(open) => setEditing(open ? p : null)}
          onSaved={(saved) =>
            setProfiles((profiles ?? []).map((x) => (x.id === saved.id ? saved : x)))
          }
          onRemove={() => {
            void actions
              .removeClaudexProfile(p.id)
              .then(() => setProfiles((profiles ?? []).filter((x) => x.id !== p.id)))
              .catch(() => undefined);
          }}
        />
      ))}
      {profiles !== null && profiles.length === 0 && editing !== "new" && (
        <div className="settings-value dim">
          none yet — add one pointed at a local bridge (e.g. claude-code-proxy)
        </div>
      )}
      {editing === "new" && (
        <ClaudexProfileForm
          initial={CLAUDEX_PRESET}
          hasAuthToken={false}
          submitLabel="create profile"
          onSubmit={(input) => actions.addClaudexProfile(input)}
          onSaved={(saved) => {
            setProfiles([...(profiles ?? []), saved]);
            setEditing(null);
          }}
        />
      )}
    </div>
  );
}

function ClaudexProfileRow({
  profile,
  editing,
  onEdit,
  onSaved,
  onRemove,
}: {
  profile: ClaudexProfileInfo;
  editing: boolean;
  onEdit: (open: boolean) => void;
  onSaved: (profile: ClaudexProfileInfo) => void;
  onRemove: () => void;
}) {
  const { actions } = useStore();
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);

  async function test() {
    setTesting(true);
    setResult(null);
    try {
      const { sidecar } = await actions.testClaudexProfile(profile.id);
      setResult({
        ok: true,
        text:
          sidecar === "managed"
            ? "bridge running (started by threadknot)"
            : "bridge reachable (started elsewhere)",
      });
    } catch (e) {
      setResult({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setTesting(false);
    }
  }

  return (
    <div className="claudex-profile">
      <div className="settings-row">
        <span className="settings-value settings-agent">
          <AgentMark agent="claudex" size={12} />
          {profile.name}
        </span>
        <span className="settings-row claudex-actions">
          <button
            type="button"
            className="settings-toggle"
            disabled={testing}
            onClick={() => void test()}
          >
            {testing ? "testing…" : "test"}
          </button>
          <button type="button" className="settings-toggle" onClick={() => onEdit(!editing)}>
            {editing ? "close" : "edit"}
          </button>
          <button type="button" className="settings-toggle" onClick={onRemove}>
            remove
          </button>
        </span>
      </div>
      <div className="settings-value dim">
        {profile.model} · {profile.baseUrl}
        {profile.sidecar ? ` · sidecar: ${profile.sidecar.command}` : ""}
      </div>
      {result && (
        <div className={`settings-value ${result.ok ? "ok" : "notify-failed"}`}>{result.text}</div>
      )}
      {editing && (
        <ClaudexProfileForm
          initial={{
            name: profile.name,
            baseUrl: profile.baseUrl,
            model: profile.model,
            smallModel: profile.smallModel ?? undefined,
            contextWindow: profile.contextWindow ?? undefined,
            efforts: profile.efforts,
            defaultEffort: profile.defaultEffort ?? undefined,
            sidecar: profile.sidecar ?? null,
          }}
          hasAuthToken={profile.hasAuthToken}
          submitLabel="save"
          onSubmit={(input) => actions.updateClaudexProfile(profile.id, input)}
          onSaved={(saved) => {
            onSaved(saved);
            onEdit(false);
          }}
        />
      )}
    </div>
  );
}

function ClaudexProfileForm({
  initial,
  hasAuthToken,
  submitLabel,
  onSubmit,
  onSaved,
}: {
  initial: ClaudexProfileInput;
  hasAuthToken: boolean;
  submitLabel: string;
  onSubmit: (input: ClaudexProfileInput) => Promise<ClaudexProfileInfo>;
  onSaved: (profile: ClaudexProfileInfo) => void;
}) {
  const [name, setName] = useState(initial.name ?? "");
  const [baseUrl, setBaseUrl] = useState(initial.baseUrl ?? "");
  const [model, setModel] = useState(initial.model ?? "");
  const [smallModel, setSmallModel] = useState(initial.smallModel ?? "");
  const [contextWindow, setContextWindow] = useState(
    initial.contextWindow ? String(initial.contextWindow) : "",
  );
  const [efforts, setEfforts] = useState((initial.efforts ?? []).join(", "));
  const [defaultEffort, setDefaultEffort] = useState(initial.defaultEffort ?? "");
  const [token, setToken] = useState("");
  const [useSidecar, setUseSidecar] = useState(!!initial.sidecar);
  const [sidecarCommand, setSidecarCommand] = useState(initial.sidecar?.command ?? "");
  const [sidecarArgs, setSidecarArgs] = useState((initial.sidecar?.args ?? []).join(" "));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function save() {
    if (busy || !name.trim() || !baseUrl.trim() || !model.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const parsedWindow = Number.parseInt(contextWindow, 10);
      const saved = await onSubmit({
        name: name.trim(),
        baseUrl: baseUrl.trim(),
        model: model.trim(),
        smallModel: smallModel.trim(),
        // 0 tells the server "blank — detect it", which it can only act on if
        // we actually send the field.
        contextWindow: Number.isFinite(parsedWindow) && parsedWindow > 0 ? parsedWindow : 0,
        efforts: efforts
          .split(",")
          .map((e) => e.trim())
          .filter(Boolean),
        defaultEffort: defaultEffort.trim(),
        // Blank means "leave the stored token alone" — the field is write-only.
        ...(token ? { authToken: token } : {}),
        sidecar: useSidecar
          ? {
              command: sidecarCommand.trim(),
              args: sidecarArgs.split(/\s+/).filter(Boolean),
            }
          : null,
      });
      onSaved(saved);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="claudex-form">
      <label className="claudex-field">
        <span>name</span>
        <input value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </label>
      <label className="claudex-field">
        <span>bridge URL</span>
        <input
          value={baseUrl}
          onChange={(e) => setBaseUrl(e.target.value)}
          placeholder="http://127.0.0.1:18765"
        />
      </label>
      <label className="claudex-field">
        <span>model</span>
        <input
          value={model}
          onChange={(e) => setModel(e.target.value)}
          placeholder="gpt-5.6-sol"
        />
      </label>
      <label className="claudex-field">
        <span>small model</span>
        <input
          value={smallModel}
          onChange={(e) => setSmallModel(e.target.value)}
          placeholder="gpt-5.6-luna"
        />
      </label>
      <label className="claudex-field">
        <span>context window</span>
        <input
          value={contextWindow}
          onChange={(e) => setContextWindow(e.target.value)}
          placeholder="blank = detect from the provider"
          inputMode="numeric"
        />
      </label>
      <label className="claudex-field">
        <span>efforts</span>
        <input
          value={efforts}
          onChange={(e) => setEfforts(e.target.value)}
          placeholder="low, medium, high"
        />
      </label>
      <label className="claudex-field">
        <span>default effort</span>
        <input value={defaultEffort} onChange={(e) => setDefaultEffort(e.target.value)} />
      </label>
      <label className="claudex-field">
        <span>auth token</span>
        <input
          type="password"
          value={token}
          onChange={(e) => setToken(e.target.value)}
          placeholder={hasAuthToken ? "stored — type to replace" : "usually not needed"}
        />
      </label>
      <label className="claudex-field claudex-check">
        <input
          type="checkbox"
          checked={useSidecar}
          onChange={(e) => setUseSidecar(e.target.checked)}
        />
        <span>threadknot starts the bridge (loopback URLs only)</span>
      </label>
      {useSidecar && (
        <>
          <label className="claudex-field">
            <span>command</span>
            <input
              value={sidecarCommand}
              onChange={(e) => setSidecarCommand(e.target.value)}
              placeholder="claude-code-proxy"
            />
          </label>
          <label className="claudex-field">
            <span>arguments</span>
            <input
              value={sidecarArgs}
              onChange={(e) => setSidecarArgs(e.target.value)}
              placeholder="serve"
            />
          </label>
        </>
      )}
      {error && <div className="settings-value notify-failed">{error}</div>}
      <button
        type="button"
        className="settings-toggle"
        disabled={busy || !name.trim() || !baseUrl.trim() || !model.trim()}
        onClick={() => void save()}
      >
        {busy ? "saving…" : submitLabel}
      </button>
    </div>
  );
}

/** True while an operation is claimed but not finished. */
function isRunning(u: UpdateStatus): boolean {
  return !!u.operation && !u.operation.finishedAt;
}

/** Which route a machine takes to a newer build. A peer on a Threadknot older
 *  than the release channel reports none, and only ever had the source one. */
function updateChannel(u: UpdateStatus): "release" | "source" {
  return u.channel ?? "source";
}

/** Short human read of where a machine sits relative to the newest build it
 *  could be running. A running operation wins: showing "needs a rebuild" while
 *  the rebuild is compiling reads as if nothing happened. */
function updateSummary(u: UpdateStatus): string {
  if (isRunning(u)) return u.operation!.stage;
  // A newer binary already on disk outranks everything on either channel: the
  // version in the footer is genuinely not the newest one here yet.
  if (u.restartPending) return "restart to finish updating";
  if (updateChannel(u) === "release") {
    if (!u.release) return "cannot reach the releases page";
    return u.releaseAvailable ? `v${u.release.version} available` : "up to date";
  }
  // No master version means the comparison never happened (no checkout, fetch
  // failed, no origin/master yet). Saying "up to date" there would be a lie.
  if (!u.repoAvailable || !u.masterVersion) return "cannot check";
  if (!u.updateAvailable) return "up to date";
  // Whichever axis is further behind is the honest headline: the binary can be
  // current while the checkout still has commits to pull, and vice versa.
  const n = Math.max(u.behindBy, u.headBehind);
  const what = n === 1 ? "1 version behind" : `${n} versions behind`;
  if (u.rebuildPending) return `${what}, needs a rebuild`;
  if (u.canFastForward) return `${what}, needs a pull`;
  return `${what}, pull blocked`;
}

/** What the card says between the click and the server's first progress
 *  broadcast. Without these the card sat unchanged for a second or two and the
 *  click read as a no-op. */
const CLAIMING: Record<"pull" | "rebuild" | "restart" | "install", string> = {
  pull: "asking this machine to pull…",
  rebuild: "starting the build…",
  restart: "restarting. this window reconnects on its own",
  install: "starting the download…",
};

/** Which button owns a running server operation. The chained pull-build-restart
 *  reports under its own kind, but the button that started it is still the pull
 *  one, and it has to keep showing progress for the whole run rather than going
 *  idle the moment the pull stage ends. */
const OP_ACTION: Record<string, "pull" | "rebuild" | "restart" | "install"> = {
  pull: "pull",
  update: "pull",
  rebuild: "rebuild",
  restart: "restart",
  install: "install",
};

/** Bytes as something a download line can say. */
function fileSize(bytes: number): string {
  if (!bytes) return "";
  const mb = bytes / 1024 / 1024;
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${Math.round(mb)} MB`;
}

/** Version + pending-commit view for one machine, with the actions that machine
 *  can safely take. Local when `machineId` is undefined. */
function UpdateCard({
  status,
  machineId,
  name,
  isLocal,
  onActed,
}: {
  status: UpdateStatus;
  machineId?: string;
  name: string;
  isLocal: boolean;
  /** Called after a successful action so a remote card can re-read itself. The
   *  local card needs no such hook: the server broadcasts its own new state. */
  onActed?: () => void;
}) {
  const { state, actions } = useStore();
  type Action = "pull" | "rebuild" | "restart" | "install";
  const [busy, setBusy] = useState<Action | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Restart stops the running app, so it takes a second click to confirm. */
  const [armed, setArmed] = useState<Action | null>(null);
  const [open, setOpen] = useState(isLocal);

  const look = machineLook(state, machineId);
  const running = isRunning(status);
  const op = status.operation;
  const channel = updateChannel(status);
  const release = status.release ?? null;
  // The button only exists where the machine can finish the job by itself. A
  // .deb install, or a release with no artifact for this platform, gets the
  // release page instead — which is the honest answer, not a dead button.
  const canInstall = Boolean(
    channel === "release" && status.releaseAvailable && status.releaseInstallSupported,
  );
  // What this machine is doing right now, whether this client started it or
  // another one did. Server state wins once it lands; `busy` only covers the
  // gap before the first broadcast. Without this the button sat disabled while
  // still reading "rebuild", which looks broken rather than busy.
  const activeAction: Action | null = busy ?? (running ? (OP_ACTION[op!.kind] ?? null) : null);
  // Where the machine can finish the job on its own, the pull button does the
  // lot. A peer that cannot rebuild or restart itself keeps the old single-step
  // button, because offering it a chain it cannot complete would just strand it
  // halfway.
  const canChain = !!status.rebuildSupported && !!status.restartSupported;
  // An older peer reports no thread count at all. Treating "unknown" as "none"
  // is the safe read: we only ever use it to add a warning, never to unlock an
  // action, and the target machine re-checks for real before it restarts.
  const busyThreads = status.activeWork ?? 0;
  /** Which operation the card knew about when the button was pressed. */
  const opId = op?.id;
  const opIdAtClick = useRef<string | undefined>(undefined);

  // Hand the button back only once the server is visibly in charge. These calls
  // resolve when the job is *claimed*, not when it finishes (a rebuild compiles
  // for minutes), so clearing on resolve re-enabled the button during the gap
  // before the first progress broadcast, which is exactly what let a second
  // and third click through.
  useEffect(() => {
    if (!busy || busy === "restart") return;
    // A server operation we had not seen at click time is proof the click
    // landed, whether it is still running or already finished. Waiting only for
    // a *running* one missed jobs quick enough to start and finish between two
    // broadcasts (a pull usually is), leaving the card claiming to work for the
    // whole fallback window after it was actually done.
    if (opId && opId !== opIdAtClick.current) {
      setBusy(null);
      return;
    }
    // Last resort, for a broadcast that never arrives at all.
    const t = window.setTimeout(() => setBusy(null), 20000);
    return () => window.clearTimeout(t);
  }, [busy, opId]);

  async function run(which: Action, force = false) {
    if (busy || running) return;
    opIdAtClick.current = opId;
    setBusy(which);
    setError(null);
    try {
      if (which === "pull") await actions.pullUpdate(machineId, canChain, force);
      else if (which === "rebuild") await actions.rebuildUpdate(machineId);
      else if (which === "install") await actions.installUpdate(machineId, force);
      else await actions.restartUpdate(machineId, force);
      setArmed(null);
      onActed?.();
      // `busy` stays set on purpose; the effect above releases it. Restart never
      // releases: the app is going down, so a live-again button would just be
      // inviting a click that cannot land.
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      // Only a refused request frees the button, because nothing was started.
      setBusy(null);
    }
  }

  /** Two-step confirm for anything that stops the app mid-flight. */
  function confirmable(which: Action, label: string, danger: string, working: string) {
    const isArmed = armed === which;
    return (
      <button
        type="button"
        className={`settings-toggle${isArmed ? " danger" : " primary"}`}
        disabled={busy !== null || running}
        onClick={() => (isArmed ? void run(which, true) : setArmed(which))}
        onBlur={() => setArmed(null)}
      >
        {activeAction === which && <span className="update-spinner" aria-hidden="true" />}
        {activeAction === which ? working : isArmed ? danger : label}
      </button>
    );
  }

  return (
    <div className={`update-card${status.updateAvailable ? " stale" : ""}`}>
      <div className="update-card-head">
        <MachineAvatar {...look} size={32} className="update-card-avatar" />
        <div className="update-card-main">
          <div className="update-card-name">
            <span className="update-card-title">{name}</span>
            {isLocal && <span className="machine-badge">this machine</span>}
          </div>
          <div className="update-card-sub">
            v{status.runningVersion}
            {channel === "release"
              ? status.releaseAvailable && release
                ? ` → v${release.version}`
                : ""
              : status.masterVersion && status.masterVersion !== status.runningVersion
                ? ` → master v${status.masterVersion}`
                : ""}
            {" · "}
            {updateSummary(status)}
          </div>
        </div>
        {/* Commits are the source channel's unit of change. On the release
            channel the notes on the card already say what is in the update, and
            a list of what landed on master since is a different question the
            user did not ask. */}
        {channel === "source" && status.commits.length > 0 && (
          <button
            type="button"
            className="settings-toggle"
            aria-expanded={open}
            onClick={() => setOpen((v) => !v)}
          >
            {open ? "hide changes" : `${status.commits.length} changes`}
          </button>
        )}
      </div>

      {status.error && <div className="update-note warn">{status.error}</div>}

      {open && channel === "source" && status.commits.length > 0 && (
        <ul className="update-commits">
          {status.commits.map((c) => (
            <li key={c.hash} className="update-commit">
              <code className="update-commit-hash">{c.hash}</code>
              <span className="update-commit-subject">{c.subject}</span>
              <span className="update-commit-date">{c.date}</span>
            </li>
          ))}
        </ul>
      )}

      {/* A live operation, then the verdict of the last finished one. Both are
          server state, so they survive a reload and show up on every client.
          `busy` covers the seconds before the server's first broadcast, so the
          click always produces something visible immediately. */}
      {(running || busy) && (
        <div className="update-progress" role="status" aria-live="polite">
          <span className="update-spinner" aria-hidden="true" />
          {running ? `${op!.kind}: ${op!.stage}` : CLAIMING[busy!]}
        </div>
      )}
      {!running && op?.ok === false && (
        <div className="update-note warn">
          <strong>{op.kind} failed.</strong> {op.error}
          {op.logPath && <div className="update-log-path">full log: {op.logPath}</div>}
        </div>
      )}
      {/* A run can succeed and still stop short — a build that produced nothing
          new, or a chain that declined to restart through live threads. That
          reason only ever rode in `error`, which the warn box above shows only
          on failure, so it used to end silently and look like nothing happened. */}
      {!running && op?.ok === true && op.error && (
        <div className="update-note">
          <strong>{op.stage}.</strong> {op.error}
        </div>
      )}

      {channel === "release" && (
        <>
          {!release && !status.error && (
            <div className="update-note">
              Threadknot has not been able to read the releases page yet. Use{" "}
              <strong>check now</strong> above.
            </div>
          )}
          {/* The notes are what makes an update something you agree to rather
              than something you accept. Shown only when there is one to take:
              reciting the release you are already on is noise. */}
          {status.releaseAvailable && release?.notes && (
            <div className="update-note update-release-notes">
              <strong>{release.name}</strong>
              <p>{release.notes}</p>
              <a href={release.url} target="_blank" rel="noreferrer noopener">
                full notes on the release page
              </a>
            </div>
          )}
          {status.releaseAvailable && release?.blocked && (
            <div className="update-note">
              {release.blocked}{" "}
              <a href={release.url} target="_blank" rel="noreferrer noopener">
                Open the release page
              </a>
              .
            </div>
          )}
          {canInstall && busyThreads > 0 && (
            <div className="update-note">
              {busyThreads} thread{busyThreads === 1 ? " is" : "s are"} still working or
              waiting on approval. Installing restarts Threadknot and interrupts{" "}
              {busyThreads === 1 ? "it" : "them"}.
            </div>
          )}
          {status.restartPending && (
            <div className="update-note">
              A newer Threadknot is already installed at <code>{status.binaryPath}</code>.
              This window is still running the old one until you restart.
            </div>
          )}

          <div className="update-actions">
            {canInstall &&
              (busyThreads > 0 ? (
                confirmable(
                  "install",
                  `update to v${release!.version}`,
                  `yes, interrupt ${busyThreads} and update`,
                  "updating…",
                )
              ) : (
                <button
                  type="button"
                  className="settings-toggle primary"
                  disabled={busy !== null || running}
                  onClick={() => void run("install")}
                  title={`Downloads ${release!.assetName} and restarts into it. The download keeps going if you close this window.`}
                >
                  {activeAction === "install" && (
                    <span className="update-spinner" aria-hidden="true" />
                  )}
                  {activeAction === "install"
                    ? "updating…"
                    : `update to v${release!.version}`}
                </button>
              ))}
            {status.restartPending &&
              status.restartSupported &&
              confirmable(
                "restart",
                "restart now",
                busyThreads > 0
                  ? `yes, interrupt ${busyThreads} and restart`
                  : "yes, restart Threadknot",
                "restarting…",
              )}
            {release && !status.releaseAvailable && !status.restartPending && (
              <span className="update-ok">latest release</span>
            )}
            {status.releaseAvailable && !status.releaseInstallSupported && (
              <a
                className="settings-toggle"
                href={release!.url}
                target="_blank"
                rel="noreferrer noopener"
              >
                download v{release!.version}
              </a>
            )}
            {canInstall && release!.assetSize > 0 && (
              <span className="update-blocked">
                {release!.assetName} · {fileSize(release!.assetSize)}
              </span>
            )}
          </div>
        </>
      )}

      {channel === "source" && status.repoAvailable && (
        <>
          {/* Only explain what is blocking a pull when there is one to do. */}
          {status.updateAvailable && status.headAhead > 0 && (
            <div className="update-note">
              Branch <code>{status.branch}</code> has {status.headAhead} commit
              {status.headAhead === 1 ? "" : "s"} that master does not. Your work stays
              put: Threadknot will not rebase, merge, reset or discard it. Push or land it
              first, then pull.
            </div>
          )}
          {status.updateAvailable && status.dirty && status.headAhead === 0 && (
            <div className="update-note">
              This checkout has uncommitted changes. Commit or stash them, then pull.
            </div>
          )}
          {status.restartPending && (
            <div className="update-note">
              A newer Threadknot is already built at <code>{status.binaryPath}</code>. This
              window is still running the old one until you restart.
            </div>
          )}
          {status.restartPending && busyThreads > 0 && (
            <div className="update-note">
              {busyThreads} thread{busyThreads === 1 ? " is" : "s are"} still working or
              waiting on approval. Restarting interrupts{" "}
              {busyThreads === 1 ? "it" : "them"}.
            </div>
          )}
          {status.rebuildPending && !status.rebuildSupported && (
            <div className="update-note">
              {/* A peer on a Threadknot older than this feature sends no
                  rebuildSupported at all. Blaming its platform would send the
                  user chasing the wrong problem: it just needs updating once by
                  hand, after which the button appears. */}
              {status.rebuildSupported === undefined
                ? "This machine is running a Threadknot from before the rebuild button existed."
                : "Automatic rebuild is not wired up on this platform yet."}{" "}
              Run <code>npx tauri build --no-bundle</code> in{" "}
              <code>{status.repoPath}</code> and reopen Threadknot.
            </div>
          )}

          <div className="update-actions">
            {/* A chain that ends in a restart needs the same consent the restart
                button asks for, so with threads live the click arms instead of
                firing. Without this the run would build for minutes and then
                decline to restart every single time, which is the state an
                agent-driven machine is usually in. */}
            {status.canFastForward &&
              (canChain && busyThreads > 0 ? (
                confirmable(
                  "pull",
                  "pull, build & restart",
                  `yes, interrupt ${busyThreads} and update`,
                  "updating…",
                )
              ) : (
                <button
                  type="button"
                  className="settings-toggle primary"
                  disabled={busy !== null || running}
                  onClick={() => void run("pull")}
                  title={
                    canChain
                      ? "Pulls master, rebuilds, and restarts into the new build. The build takes minutes and keeps going if you close this window."
                      : "Fast-forwards this checkout to master. Rebuilding is a separate step."
                  }
                >
                  {activeAction === "pull" && (
                    <span className="update-spinner" aria-hidden="true" />
                  )}
                  {activeAction === "pull"
                    ? canChain
                      ? "updating…"
                      : "pulling…"
                    : canChain
                      ? "pull, build & restart"
                      : "pull to master"}
                </button>
              ))}
            {status.updateAvailable &&
              !status.canFastForward &&
              !status.rebuildPending &&
              !status.restartPending && (
                <span className="update-blocked">pull blocked, see above</span>
              )}
            {/* Only claim a clean bill of health when the comparison actually
                ran. A machine that cannot reach origin/master reports no update
                and zero commits behind, which is indistinguishable from being
                current unless we check that a real answer came back. Without
                this a red "cannot check" error sits directly above a green
                "matches master". */}
            {status.repoAvailable &&
              !!status.masterVersion &&
              !status.updateAvailable &&
              !status.restartPending &&
              status.headBehind === 0 && <span className="update-ok">matches master</span>}
            {status.rebuildPending && status.rebuildSupported && (
              <button
                type="button"
                className="settings-toggle primary"
                disabled={busy !== null || running}
                onClick={() => void run("rebuild")}
                title="Compiles in the background. Nothing restarts until you say so."
              >
                {activeAction === "rebuild" && (
                  <span className="update-spinner" aria-hidden="true" />
                )}
                {activeAction === "rebuild" ? "building…" : "rebuild"}
              </button>
            )}
            {status.restartPending &&
              status.restartSupported &&
              confirmable(
                "restart",
                "restart now",
                busyThreads > 0
                  ? `yes, interrupt ${busyThreads} and restart`
                  : "yes, restart Threadknot",
                "restarting…",
              )}
            {status.restartPending && !status.restartSupported && (
              <span className="update-blocked">
                close and reopen Threadknot to load the new build
              </span>
            )}
          </div>
        </>
      )}

      {error && <div className="update-note warn">{error}</div>}
    </div>
  );
}

/**
 * Where this machine gets a newer Threadknot from.
 *
 * Offered as a choice, not just reported, because the derived answer is right
 * almost always and wrong in the one case that matters most: the machine the
 * releases are cut on, where the only way to find out what an update actually
 * does to a user is to take one. Machine-local, so pinning this box changes
 * nothing about the rest of the fleet.
 */
function ChannelField({ status }: { status: UpdateStatus }) {
  const { actions } = useStore();
  const [saving, setSaving] = useState<"release" | "source" | "auto" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const channel = updateChannel(status);

  async function pick(next: "release" | "source" | "auto") {
    setSaving(next);
    setError(null);
    try {
      await actions.setUpdateChannel(next);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(null);
    }
  }

  return (
    <div className="update-channel">
      <div className="settings-row">
        <span className="settings-value dim">this machine updates from</span>
        <div className="update-channel-picker">
          <button
            type="button"
            className={`settings-toggle${channel === "release" ? " primary" : ""}`}
            disabled={saving !== null}
            onClick={() => void pick("release")}
            title="Published builds from the releases page: downloaded, installed and restarted for you."
          >
            releases
          </button>
          <button
            type="button"
            className={`settings-toggle${channel === "source" ? " primary" : ""}`}
            disabled={saving !== null}
            onClick={() => void pick("source")}
            title="This machine's own checkout: pull master, rebuild, restart."
          >
            source
          </button>
        </div>
      </div>
      <div className="settings-value dim">
        {channel === "release"
          ? "Threadknot downloads the published build for this platform, installs it over this copy, and restarts."
          : "Threadknot fast-forwards this machine's checkout to master, rebuilds it, and restarts."}
        {status.channelPinned ? (
          <>
            {" "}
            <button
              type="button"
              className="settings-toggle update-channel-auto"
              disabled={saving !== null}
              onClick={() => void pick("auto")}
            >
              choose automatically
            </button>
          </>
        ) : (
          " Chosen automatically, because this machine " +
          (status.repoAvailable ? "has a source checkout." : "has no source checkout.")
        )}
      </div>
      {error && <div className="update-note warn">{error}</div>}
    </div>
  );
}

/** Point this machine at its Threadknot source checkout. Needed when the build-time
 *  path is gone (the folder moved, or the binary was copied from another
 *  machine or another operating system). Machine-local: this is a fact about
 *  one disk, so it is never replicated to peers. */
function RepoPathField({ status }: { status: UpdateStatus }) {
  const { actions } = useStore();
  const [draft, setDraft] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);

  async function save() {
    setSaving(true);
    setError(null);
    try {
      await actions.setUpdateRepoPath(draft.trim());
      setEditing(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  // Offered unprompted only when there is nothing to work with; otherwise it is
  // a link that gets out of the way.
  if (!editing && status.repoAvailable) {
    return (
      <button
        type="button"
        className="settings-toggle update-repo-edit"
        onClick={() => {
          setDraft(status.repoPath);
          setEditing(true);
        }}
      >
        change source folder
      </button>
    );
  }

  return (
    <div className="update-repo-field">
      <label className="settings-value dim" htmlFor="update-repo-path">
        Threadknot source folder on this machine
      </label>
      <div className="update-repo-row">
        <input
          id="update-repo-path"
          className="settings-input"
          value={draft}
          spellCheck={false}
          placeholder="/home/you/projects/threadknot"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && void save()}
        />
        <button
          type="button"
          className="settings-toggle primary"
          disabled={saving || !draft.trim()}
          onClick={() => void save()}
        >
          {saving ? "saving…" : "save"}
        </button>
        {status.repoAvailable && (
          <button type="button" className="settings-toggle" onClick={() => setEditing(false)}>
            cancel
          </button>
        )}
      </div>
      {error && <div className="update-note warn">{error}</div>}
    </div>
  );
}

/** Version drift across the fleet: is each machine on the newest master, and is
 *  its running binary actually built from it? */
function UpdatesSettings() {
  const { state, actions } = useStore();
  const local = state.update;
  // `unreachable`, not `error`: UpdateStatus carries its own optional `error`
  // field, so a key named `error` cannot distinguish the two cases.
  const [fleet, setFleet] = useState<
    Record<string, UpdateStatus | { unreachable: string }>
  >({});
  const [checking, setChecking] = useState(false);

  const online = state.peers.filter((p) => p.online);
  const onlineIds = online.map((p) => p.machineId).join(",");

  // Paint from cache so the panel is never blank, then immediately confirm it
  // against the repo as it is now. The cached snapshot can be a full
  // POLL_INTERVAL stale, and every blocked message here tells you to go fix
  // something outside Threadknot ("commit or stash them, then pull"). Coming back
  // to a panel still reciting a blocker you just cleared is therefore the
  // normal path through this screen, not an edge case, and nothing on it
  // suggests the answer is old or that "check now" is what you need.
  useEffect(() => {
    void actions.refreshUpdate().catch(() => undefined);
    void checkNow();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Ask one peer. Cached server-side, so this is a cheap round trip.
  const loadPeer = useCallback(
    async (machineId: string) => {
      try {
        const st = await actions.updateStatusFor(machineId);
        setFleet((m) => ({ ...m, [machineId]: st }));
      } catch (e: unknown) {
        const msg = e instanceof Error ? e.message : String(e);
        // A machine old enough to predate this feature cannot answer — which
        // is itself the answer: it is behind and only a manual rebuild fixes it.
        const friendly = msg.includes("unknown request type")
          ? "running a Threadknot too old to report its version. Pull and rebuild it by hand once, then it appears here."
          : msg;
        setFleet((m) => ({ ...m, [machineId]: { unreachable: friendly } }));
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  useEffect(() => {
    for (const p of online) void loadPeer(p.machineId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [onlineIds]);

  // The answer arrives on the `updates` broadcast, not from the call, so the
  // button has to watch for a genuinely newer reading. A fixed timer used to
  // stand in for this and would call it done while the fetch was still running.
  const checkedAt = local?.checkedAt;
  const checkedBefore = useRef<string | undefined>(undefined);

  useEffect(() => {
    if (!checking) return;
    if (checkedAt && checkedAt !== checkedBefore.current) {
      setChecking(false);
      return;
    }
    // A fetch against an unreachable origin can hang past the network leash;
    // stop claiming to be busy rather than spinning forever.
    const t = window.setTimeout(() => setChecking(false), 20000);
    return () => window.clearTimeout(t);
  }, [checking, checkedAt]);

  async function checkNow() {
    checkedBefore.current = local?.checkedAt;
    setChecking(true);
    await actions.checkForUpdate().catch(() => undefined);
  }

  return (
    <>
      <div className="settings-block">
        <div className="settings-row">
          <span className="settings-label">updates</span>
          <button
            type="button"
            className="settings-toggle update-check"
            disabled={checking}
            onClick={() => void checkNow()}
          >
            {checking && <span className="update-spinner" aria-hidden="true" />}
            {checking ? "checking…" : "check now"}
          </button>
        </div>
        {local ? (
          <>
            <UpdateCard
              status={local}
              name={state.hello?.friendlyName ?? "this machine"}
              isLocal
            />
            <div className="settings-value dim update-checked">
              last checked {timeAgo(local.checkedAt)}
              {updateChannel(local) === "source" && local.repoPath
                ? ` · ${local.repoPath}${local.repoSource ? ` (${local.repoSource})` : ""}`
                : ""}
            </div>
            <ChannelField status={local} />
            {updateChannel(local) === "source" && <RepoPathField status={local} />}
          </>
        ) : (
          <div className="settings-value dim">loading…</div>
        )}
      </div>

      <div className="settings-block">
        <div className="settings-label">other machines</div>
        {online.length === 0 ? (
          <div className="settings-value dim">no other machines online</div>
        ) : (
          online.map((p) => {
            const st = fleet[p.machineId];
            if (!st) return <div key={p.machineId} className="settings-value dim">{p.name}: checking…</div>;
            if ("unreachable" in st)
              return (
                <div key={p.machineId} className="update-note warn">
                  {p.name}: {st.unreachable}
                </div>
              );
            return (
              <UpdateCard
                key={p.machineId}
                status={st}
                machineId={p.machineId}
                name={p.name}
                isLocal={false}
                // A routed action runs on the peer, so the peer broadcasts to its
                // own clients, not to us. Re-read it or this card keeps showing
                // the state the user just fixed. The delay lets a restart settle;
                // if it has not, the row honestly reads as unreachable.
                onActed={() => window.setTimeout(() => void loadPeer(p.machineId), 3000)}
              />
            );
          })
        )}
      </div>
    </>
  );
}

/**
 * Signed-in browser profiles.
 *
 * The user signs in by hand in the Browser tab; Threadknot keeps the session that
 * results, never a password. Because a signed-in browser can act as them, each
 * profile is limited to the sites it was made for, and the browser itself
 * refuses to load anything else.
 */
/** Favicon for a login card. The first real (non-`*`) origin gets a fetched
 *  favicon; an unscoped login gets a generic globe. The favicon lookup is the
 *  one thing here that leaves the machine — noted in the panel copy. */
function loginFavicon(origins: string[]): string | null {
  const host = origins.find((o) => o && o !== "*");
  if (!host) return null;
  const clean = host.replace(/^https?:\/\//, "").replace(/^\*\./, "").split("/")[0];
  return `https://www.google.com/s2/favicons?domain=${encodeURIComponent(clean)}&sz=32`;
}

function BrowserProfileSettings() {
  const { state, actions } = useStore();
  const [profiles, setProfiles] = useState<BrowserProfileInfo[] | null>(null);
  const [name, setName] = useState("");
  const [sites, setSites] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  /** Whose logins are being managed: undefined = this machine, else a peer. */
  const [machine, setMachine] = useState<string | undefined>(undefined);
  const [loadError, setLoadError] = useState("");
  /** Security gate state from the list response. */
  const [keyPresent, setKeyPresent] = useState(false);
  const [requireKey, setRequireKey] = useState(false);
  /** Fallback reveal when no key is enrolled: armed → confirmed. */
  const [reveal, setReveal] = useState(false);
  /** Which card is expanded to its full detail. */
  const [expanded, setExpanded] = useState<string | null>(null);

  const localId = state.hello?.machineId;
  const localName = state.hello?.friendlyName ?? "this machine";
  const isLocal = machine === undefined || machine === localId;
  const machineName = isLocal
    ? localName
    : state.peers.find((p) => p.machineId === machine)?.name ?? "that machine";

  const reload = useCallback(() => {
    setProfiles(null);
    setLoadError("");
    void actions
      .listBrowserProfiles(machine)
      .then((res) => {
        setProfiles(res.profiles);
        setKeyPresent(res.keyPresent);
        setRequireKey(res.requireKey);
      })
      .catch((e) => {
        setProfiles([]);
        setLoadError(e instanceof Error ? e.message : String(e));
      });
  }, [actions, machine]);

  useEffect(reload, [reload]);

  // While gated on the key, re-poll so plugging it in unlocks the view without
  // reopening Settings. The server also broadcasts on removal, but polling
  // covers insertion cleanly and stops once unlocked.
  const gatedByKey = requireKey && !keyPresent;
  useEffect(() => {
    if (!gatedByKey) return;
    const t = window.setInterval(reload, 2000);
    return () => window.clearInterval(t);
  }, [gatedByKey, reload]);

  // The panel is locked when the key gate is active and the key is out, or —
  // when no key is enrolled — until the owner clicks Reveal.
  const noKeyEnrolled = !requireKey && !keyPresent;
  const locked = gatedByKey || (noKeyEnrolled && !reveal);

  const create = async () => {
    setBusy(true);
    setError("");
    try {
      await actions.createBrowserProfile(
        name,
        sites.split(/[\s,]+/).filter(Boolean),
        machine,
      );
      setName("");
      setSites("");
      reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  if (locked) {
    return (
      <div className="settings-block">
        <div className="bl-lock">
          <UsbKeyIcon size={52} className={`bl-lock-key${keyPresent ? " present" : ""}`} />
          {gatedByKey ? (
            <>
              <div className="bl-lock-title">Insert your security key</div>
              <div className="bl-lock-text">
                Saved logins are locked without it. Plug your key in to view them —
                this unlocks the moment it’s detected.
              </div>
            </>
          ) : (
            <>
              <div className="bl-lock-title">Saved logins are protected</div>
              <div className="bl-lock-text">
                These are live signed-in sessions. Reveal them to view or manage.
              </div>
              <button type="button" className="bl-create-btn" onClick={() => setReveal(true)}>
                Reveal saved logins
              </button>
            </>
          )}
        </div>
      </div>
    );
  }

  return (
    <>
      <div className="settings-block">
        <div className="settings-label">how it works</div>
        <div className="bl-steps">
          <div className="bl-step">
            <span className="bl-step-num">1</span>
            <div>
              <div className="bl-step-title">Create a login</div>
              <div className="bl-step-text">
                Name it after the account it will hold — “Wave”, “GitHub”, or
                just your own name. A login belongs to one machine’s browser;
                pick the machine below.
              </div>
            </div>
          </div>
          <div className="bl-step">
            <span className="bl-step-num">2</span>
            <div>
              <div className="bl-step-title">Attach it to a chat</div>
              <div className="bl-step-text">
                Open the Browser tab in a chat on that machine and pick the
                login from the dropdown in its toolbar.
              </div>
            </div>
          </div>
          <div className="bl-step">
            <span className="bl-step-num">3</span>
            <div>
              <div className="bl-step-title">Sign in once, in the pane</div>
              <div className="bl-step-text">
                You type the password, not the agent — Threadknot keeps the
                signed-in session, never your credentials. A chat on another
                machine shows that machine’s browser right here, so you can
                sign it in without walking over to it.
              </div>
            </div>
          </div>
        </div>
      </div>

      <div className="settings-block">
        <div className="settings-label">machine</div>
        <div className="archive-machines">
          <button
            type="button"
            className={`archive-chip${isLocal ? " on" : ""}`}
            aria-current={isLocal}
            onClick={() => setMachine(undefined)}
          >
            <MachineAvatar {...machineLook(state, undefined)} size={18} preview={false} />
            <span className="archive-chip-name">{localName}</span>
            <span className="archive-chip-dot online" title="online" />
          </button>
          {state.peers.map((p) => (
            <button
              key={p.machineId}
              type="button"
              className={`archive-chip${machine === p.machineId ? " on" : ""}`}
              aria-current={machine === p.machineId}
              onClick={() => setMachine(p.machineId)}
            >
              <MachineAvatar {...machineLook(state, p.machineId)} size={18} preview={false} />
              <span className="archive-chip-name">{p.name}</span>
              <span
                className={`archive-chip-dot${p.online ? " online" : ""}`}
                title={p.online ? "online" : "offline"}
              />
            </button>
          ))}
        </div>
      </div>

      {isLocal && (
        <div className="settings-block">
          <div className="settings-label">security</div>
          <button
            type="button"
            className={`browser-vault-keytoggle${requireKey ? " on" : ""}${keyPresent ? " present" : ""}`}
            aria-pressed={requireKey}
            disabled={!requireKey && !keyPresent}
            onClick={() =>
              void actions
                .setBrowserLoginSecurity(!requireKey, machine)
                .then(() => setRequireKey(!requireKey))
                .catch((e) => setError(e instanceof Error ? e.message : String(e)))
            }
          >
            <UsbKeyIcon className="browser-vault-keyicon" />
            <span className="browser-vault-keycopy">
              <strong>Require my security key to open saved logins</strong>
              <em>
                {keyPresent ? "key inserted" : "no key detected — plug it in to enable"}
                {requireKey ? " · removing it locks this panel and your logins" : ""}
              </em>
            </span>
            <span className="browser-vault-keystate">{requireKey ? "ON" : "OFF"}</span>
          </button>
        </div>
      )}

      <div className="settings-block">
        <div className="settings-label">
          {isLocal ? "logins on this machine" : `logins on ${machineName}`}
        </div>
        {loadError && <div className="bl-empty">{loadError}</div>}
        {!loadError && profiles && profiles.length === 0 && (
          <div className="bl-empty">
            No browser logins on {isLocal ? "this machine" : machineName} yet.
            Create one below, then attach it from a chat’s Browser tab.
          </div>
        )}
        {(profiles ?? []).map((profile) => {
          const isOpen = expanded === profile.id;
          const live = (profile.liveOn?.length ?? 0) > 0;
          const favicon = loginFavicon(profile.origins);
          return (
            <div key={profile.id} className={`bl-card${isOpen ? " open" : ""}`}>
              <button
                type="button"
                className="bl-card-head"
                aria-expanded={isOpen}
                onClick={() => setExpanded(isOpen ? null : profile.id)}
              >
                <span className="bl-fav">
                  {favicon ? (
                    <img src={favicon} alt="" width={18} height={18} />
                  ) : (
                    <GlobeIcon size={16} />
                  )}
                </span>
                <span className="bl-card-name">{profile.name}</span>
                {live && (
                  <span className="bl-live" title={`open in ${profile.liveOn?.length} chat(s) now`}>
                    <span className="status-dot st-running" />
                    open now
                  </span>
                )}
                <span className="bl-card-used">
                  {profile.lastUsedAt ? timeAgo(profile.lastUsedAt) : "never used"}
                </span>
                <ChevronIcon size={14} open={isOpen} className="bl-chevron" />
              </button>
              {isOpen && (
                <div className="bl-card-detail">
                  <div className="bl-detail-row">
                    <span className="bl-detail-k">Sites</span>
                    <span className="bl-detail-v">
                      {profile.origins.includes("*") ? (
                        <span className="bl-scope any">any site</span>
                      ) : (
                        profile.origins.join(", ")
                      )}
                    </span>
                  </div>
                  <div className="bl-detail-row">
                    <span className="bl-detail-k">Created</span>
                    <span className="bl-detail-v">{formatFullDateTime(profile.createdAt)}</span>
                  </div>
                  <div className="bl-detail-row">
                    <span className="bl-detail-k">Last used</span>
                    <span className="bl-detail-v">
                      {profile.lastUsedAt ? formatFullDateTime(profile.lastUsedAt) : "never"}
                    </span>
                  </div>
                  <div className="bl-detail-row">
                    <span className="bl-detail-k">Machine</span>
                    <span className="bl-detail-v bl-detail-machine">
                      <MachineAvatar {...machineLook(state, profile.machineId)} size={16} preview={false} />
                      {machineName}
                    </span>
                  </div>
                  <button
                    type="button"
                    className={`bl-remove${confirmDelete === profile.id ? " confirm" : ""}`}
                    onClick={() => {
                      if (confirmDelete !== profile.id) {
                        setConfirmDelete(profile.id);
                        return;
                      }
                      setConfirmDelete(null);
                      void actions
                        .deleteBrowserProfile(profile.id, machine)
                        .then(reload)
                        .catch(() => undefined);
                    }}
                  >
                    {confirmDelete === profile.id ? "Really sign out?" : "Sign out & forget"}
                  </button>
                </div>
              )}
            </div>
          );
        })}
      </div>

      <div className="settings-block">
        <div className="settings-label">
          {isLocal ? "add a login here" : `add a login on ${machineName}`}
        </div>
        <div className="bl-form">
          <label className="bl-field">
            <span className="bl-field-label">Name</span>
            <input
              className="bl-input"
              placeholder="e.g. Wave"
              value={name}
              onChange={(e) => setName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !busy && name.trim()) void create();
              }}
            />
          </label>
          <label className="bl-field">
            <span className="bl-field-label">
              Sites it can visit <em>optional</em>
            </span>
            <input
              className="bl-input mono"
              placeholder="Leave blank to allow any site"
              value={sites}
              onChange={(e) => setSites(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !busy && name.trim()) void create();
              }}
            />
            <span className="bl-field-hint">
              To fence this browser in, list sites like{" "}
              <code>wave.com</code> or <code>*.example.com</code>. Blank means
              it can go anywhere on the web.
            </span>
          </label>
          <button
            type="button"
            className="bl-create-btn"
            disabled={busy || !name.trim()}
            onClick={() => void create()}
          >
            {busy ? "Creating…" : "Create login"}
          </button>
          {error && <div className="settings-value settings-error">{error}</div>}
        </div>
      </div>
    </>
  );
}

/** Local capture with either an on-device Whisper CLI or an explicitly
 * configured OpenAI-compatible transcription endpoint. Credentials stay
 * write-only and this screen is mounted only for the master connection. */
function VoiceSettings() {
  const { actions } = useStore();
  const [settings, setSettings] = useState<DictationSettings | null>(null);
  const [provider, setProvider] = useState<DictationProvider>("local");
  const [baseUrl, setBaseUrl] = useState("https://api.openai.com/v1");
  const [model, setModel] = useState("gpt-transcribe");
  const [apiKey, setApiKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void actions
      .getDictationSettings()
      .then((next) => {
        if (cancelled) return;
        setSettings(next);
        setProvider(next.provider);
        setBaseUrl(next.baseUrl);
        setModel(next.model);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [actions]);

  async function save() {
    if (busy || !settings) return;
    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      const next = await actions.saveDictationSettings({
        provider,
        baseUrl: baseUrl.trim(),
        model: model.trim(),
        ...(apiKey.trim() ? { apiKey: apiKey.trim() } : {}),
      });
      setSettings(next);
      setApiKey("");
      setSaved(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  if (!settings) {
    return (
      <div className="settings-block">
        <div className="settings-label">voice dictation</div>
        <div className={`settings-value ${error ? "notify-failed" : "dim"}`}>
          {error ?? "loading transcription settings…"}
        </div>
      </div>
    );
  }

  const apiReady = settings.hasApiKey || apiKey.trim().length > 0;
  const canSave =
    !busy &&
    !!baseUrl.trim() &&
    !!model.trim() &&
    (provider === "local" || apiReady);

  return (
    <div className="settings-block voice-settings">
      <div className="settings-label">voice dictation</div>
      <p className="voice-intro">
        Threadknot records this machine&apos;s microphone with ffmpeg, then turns the
        completed clip into text locally or through a compatible API.
      </p>

      {!settings.captureAvailable && (
        <div className="voice-callout bad">{settings.captureHint}</div>
      )}

      <div className="settings-row voice-provider-row">
        <span className="settings-value">
          transcription
          <span className="settings-hint">Choose where recorded audio is processed.</span>
        </span>
        <span className="settings-seg">
          <button
            type="button"
            className={`settings-toggle ${provider === "local" ? "on" : ""}`}
            onClick={() => setProvider("local")}
          >
            local
          </button>
          <button
            type="button"
            className={`settings-toggle ${provider === "api" ? "on" : ""}`}
            onClick={() => setProvider("api")}
          >
            API
          </button>
        </span>
      </div>

      {provider === "local" ? (
        <div className={`voice-callout ${settings.localAvailable ? "good" : "bad"}`}>
          {settings.localAvailable
            ? "Local Whisper is ready. Audio never leaves this machine."
            : settings.localHint}
        </div>
      ) : (
        <>
          <div className="voice-callout warn">
            Recorded audio is uploaded to this provider. Use an endpoint whose data
            handling you trust.
          </div>
          <div className="claudex-form voice-form">
            <label className="claudex-field">
              <span>base URL</span>
              <input
                type="text"
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                placeholder="https://api.openai.com/v1"
              />
            </label>
            <label className="claudex-field">
              <span>model</span>
              <input
                type="text"
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder="gpt-transcribe or whisper-1"
              />
            </label>
            <label className="claudex-field">
              <span>API key</span>
              <input
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder={settings.hasApiKey ? "stored — type to replace" : "required"}
                autoComplete="off"
              />
            </label>
          </div>
          <div className="settings-hint voice-secret-note">
            The key is stored on this machine and is never returned after saving.
            Any OpenAI-compatible <code>/audio/transcriptions</code> endpoint can be used.
          </div>
        </>
      )}

      {error && <div className="settings-value notify-failed voice-result">{error}</div>}
      {saved && !error && <div className="settings-value ok voice-result">saved</div>}
      <button
        type="button"
        className="settings-toggle primary voice-save"
        disabled={!canSave}
        onClick={() => void save()}
      >
        {busy ? "saving…" : "save voice settings"}
      </button>
    </div>
  );
}

const SETTINGS_SECTIONS = [
  { id: "appearance", label: "Appearance", blurb: "theme, size, sidebar, composer" },
  { id: "notifications", label: "Notifications", blurb: "alerts & sound" },
  { id: "machines", label: "Machines", blurb: "your fleet" },
  { id: "phone", label: "Phone & access", blurb: "LAN URL, devices" },
  { id: "agents", label: "Agents", blurb: "Claude, Codex, Kimi, Claudex" },
  { id: "voice", label: "Voice", blurb: "dictation & transcription" },
  { id: "library", label: "Library", blurb: "skills & MCP tools" },
  { id: "browser", label: "Browser logins", blurb: "stay signed in" },
  { id: "terminal", label: "Terminal", blurb: "font & cursor" },
  { id: "archives", label: "Archives", blurb: "finished threads" },
  { id: "updates", label: "Updates", blurb: "version & master" },
  { id: "about", label: "About", blurb: "licence, credits, build" },
] as const;

type SettingsSection = (typeof SETTINGS_SECTIONS)[number]["id"];

/**
 * The section panes. Desktop (nav rail) and mobile (sheet) render this same
 * component, so a section only ever exists in one place.
 */
function SettingsSectionContent({
  section,
  isTauri,
  isMaster,
  onCloseSettings,
  onUnlock,
}: {
  section: SettingsSection;
  isTauri: boolean;
  isMaster: boolean;
  onCloseSettings: () => void;
  onUnlock: () => void;
}) {
  switch (section) {
    case "appearance":
      return (
        <>
          <AppearanceStudio />
          <SkinsSettings />
          <SidebarSettings />
          <ComposerSettings />
        </>
      );
    case "terminal":
      return <TerminalSettings />;
    case "notifications":
      return <NotifySettings isTauri={isTauri} />;
    case "machines":
      return <MachinesSettings />;
    case "phone":
      return <PhoneAccessSettings />;
    case "agents":
      return <AgentsSettings />;
    case "voice":
      return isMaster ? <VoiceSettings /> : null;
    case "library":
      return <LibrarySettings />;
    case "browser":
      return <BrowserProfileSettings />;
    case "archives":
      return <ArchivesSettings onClose={onCloseSettings} />;
    case "updates":
      return <UpdatesSettings />;
    case "about":
      return <AboutSettings onUnlock={onUnlock} />;
  }
}

/** Downward pull, in px, past which releasing dismisses the mobile sheet. */
const SHEET_DISMISS_PX = 96;
/** Downward speed, in px/ms, that dismisses regardless of distance. */
const SHEET_FLICK_SPEED = 0.5;

/**
 * Pull-down-to-dismiss for the mobile sheet.
 *
 * Deliberately bound to the grip and header only. If the whole sheet took the
 * gesture, every upward scroll inside a long section (Machines, Library) would
 * be ambiguous at the top of its scroll range, and the sheet would fight the
 * content for it — the classic bottom-sheet bug. The grip is the handle in both
 * senses.
 *
 * Drag offset rides on the CSS `translate` property, which composes with — and
 * so never fights — the `transform` the open/close keyframes own.
 */
function useSheetDrag(sheetRef: React.RefObject<HTMLDivElement | null>, onDismiss: () => void) {
  const start = useRef<{ y: number; t: number } | null>(null);
  const [dragging, setDragging] = useState(false);

  const setOffset = useCallback(
    (px: number) => {
      sheetRef.current?.style.setProperty("--sheet-drag", `${px}px`);
    },
    [sheetRef],
  );

  const onPointerDown = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
    // The close and back buttons live in the drag zone; let them be buttons.
    if ((e.target as HTMLElement).closest("button")) return;
    // Without this the header text starts a native selection-drag a few pixels
    // in, which fires pointercancel and kills the gesture halfway down.
    e.preventDefault();
    start.current = { y: e.clientY, t: e.timeStamp };
    setDragging(true);
    e.currentTarget.setPointerCapture(e.pointerId);
  }, []);

  const onPointerMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      if (!start.current) return;
      const dy = e.clientY - start.current.y;
      // Upward pulls get rubber-banded rather than lifting the sheet out of
      // its slot: there is nothing above it to reveal.
      setOffset(dy > 0 ? dy : dy / 4);
    },
    [setOffset],
  );

  const onPointerUp = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      const from = start.current;
      if (!from) return;
      start.current = null;
      setDragging(false);
      const dy = e.clientY - from.y;
      const speed = dy / Math.max(1, e.timeStamp - from.t);
      if (dy > SHEET_DISMISS_PX || (speed > SHEET_FLICK_SPEED && dy > 24)) {
        // Leave the offset alone — the closing rule animates `translate` from
        // wherever the finger let go, so the sheet keeps the gesture's momentum.
        onDismiss();
      } else {
        setOffset(0);
      }
    },
    [onDismiss, setOffset],
  );

  // A cancel is the system taking the pointer away (a call arrives, the OS
  // claims the gesture). It carries no meaningful coordinates — reading
  // clientY off one gives 0, which reads as a big *upward* drag — so it can
  // never share the release path. Always put the sheet back.
  const onPointerCancel = useCallback(() => {
    if (!start.current) return;
    start.current = null;
    setDragging(false);
    setOffset(0);
  }, [setOffset]);

  return {
    dragging,
    dragHandlers: { onPointerDown, onPointerMove, onPointerUp, onPointerCancel },
  };
}

/**
 * Settings: a centred dialog with a left nav rail on desktop, and on phones
 * (≤767px) a bottom sheet whose sections are a drill-down list. The old mobile
 * treatment squeezed the rail into a sideways-scrolling row of 12 unlabelled
 * chips, which meant hunting for a section you couldn't see.
 */
export function SettingsScreen({ onClose }: { onClose: () => void }) {
  const { state } = useStore();
  const isMobile = useIsMobile();
  const visibleSections = SETTINGS_SECTIONS.filter(
    (item) => item.id !== "voice" || state.hello?.principal === "master",
  );
  // Always the first section, whatever else is going on. Settings used to open
  // on Updates whenever one was pending, which meant that for the whole life of
  // an available update every trip here — to change a theme, to add a machine —
  // landed somewhere nobody asked for. The pulsing gear already says an update
  // is waiting; it does not need to also seize the wheel.
  const [section, setSection] = useState<SettingsSection>("appearance");
  // Mobile only: the sheet always opens on the section list, for the same
  // reason.
  const [drilled, setDrilled] = useState(false);

  // What the About section can raise. While it is up it owns the whole dialog
  // and the keyboard, including Escape (which leaves it, rather than closing
  // Settings), so everything else here is unmounted for the duration.
  const [circuit, setCircuit] = useState(false);
  const [closing, setClosing] = useState(false);

  const requestClose = useCallback(() => {
    if (!closing) setClosing(true);
  }, [closing]);

  // The surface currently on screen — the sheet on mobile, the dialog on
  // desktop. Only one is mounted at a time, so they can share the ref.
  const surfaceRef = useRef<HTMLDivElement | null>(null);

  // Unmount when the exit animation actually ends, not on a timer guessing how
  // long it took. A fixed 220ms was a race: the `.closing` class only lands on
  // the next React render, so on a loaded machine the 200ms animation was still
  // mid-flight when the timer fired and Settings vanished in a pop. The timeout
  // survives only as a backstop for the case where no event ever arrives.
  useEffect(() => {
    if (!closing) return;
    const node = surfaceRef.current;
    let done = false;
    const finish = () => {
      if (done) return;
      done = true;
      onClose();
    };
    // Both events bubble, so a transition on any descendant (a hover, a
    // spinner) would otherwise cut the exit short.
    const onEnd = (e: Event) => {
      if (e.target === node) finish();
    };
    node?.addEventListener("animationend", onEnd);
    node?.addEventListener("transitionend", onEnd);
    const timer = window.setTimeout(finish, 600);
    return () => {
      node?.removeEventListener("animationend", onEnd);
      node?.removeEventListener("transitionend", onEnd);
      window.clearTimeout(timer);
    };
  }, [closing, onClose]);

  // On mobile Escape is two-stage — out of a section first, then out of
  // Settings — so it agrees with the back arrow beside it.
  useEffect(() => {
    if (circuit || closing) return;
    function onKey(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      if (isMobile && drilled) setDrilled(false);
      else requestClose();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [circuit, closing, drilled, isMobile, requestClose]);

  const { dragging, dragHandlers } = useSheetDrag(surfaceRef, requestClose);

  const current = visibleSections.find((s) => s.id === section);
  const paneContent = (
    <SettingsSectionContent
      section={section}
      isTauri={state.isTauri}
      isMaster={state.hello?.principal === "master"}
      onCloseSettings={requestClose}
      onUnlock={() => setCircuit(true)}
    />
  );

  // Shared by both branches: the theme sync and the About easter egg, which
  // takes over the whole surface when it is up.
  const shell = (
    <>
      {/* Keeps the applied custom theme in sync with server records while
          Settings is open (live edits, cross-window deletes). App.tsx should
          mount its own <ThemeSync/> at the root for boot + settings-closed. */}
      <ThemeSync />
      {circuit && <LegacyCircuit onExit={() => setCircuit(false)} />}
    </>
  );

  const mobileSheet = (
    <div
      ref={surfaceRef}
      className={
        `settings-sheet${circuit ? " ss-eclipsed" : ""}` +
        `${closing ? " closing" : ""}${dragging ? " dragging" : ""}`
      }
      data-zoom-pane="settings"
      role="dialog"
      aria-label="Settings"
      onClick={(e) => e.stopPropagation()}
    >
      {shell}
      {!circuit && (
        <>
          <div className="ss-sheet-grab" {...dragHandlers}>
            <div className="ss-grip" aria-hidden="true" />
            <header className="ss-head">
              {drilled ? (
                <button
                  type="button"
                  className="icon-btn ss-back"
                  aria-label="Back to all settings"
                  onClick={() => setDrilled(false)}
                >
                  <ChevronIcon size={16} />
                </button>
              ) : null}
              <span className={drilled ? "ss-sheet-title" : "ss-title"}>
                {drilled ? (current?.label ?? "Settings") : "SETTINGS"}
              </span>
              {!drilled && (
                <span className="ss-head-sub">{state.hello?.friendlyName ?? ""}</span>
              )}
              <button
                className="icon-btn ss-close"
                aria-label="Close settings"
                onClick={requestClose}
              >
                <XIcon size={15} />
              </button>
            </header>
          </div>
          {drilled ? (
            <div className="ss-content ss-pane ss-pane-fwd" key={section}>
              {paneContent}
            </div>
          ) : (
            <nav className="ss-menu ss-pane ss-pane-back" aria-label="Settings sections">
              {visibleSections.map((s) => (
                <button
                  key={s.id}
                  type="button"
                  className="ss-menu-item"
                  onClick={() => {
                    setSection(s.id);
                    setDrilled(true);
                  }}
                >
                  <span className="ss-menu-text">
                    <span className="ss-menu-label">{s.label}</span>
                    <span className="ss-menu-sub">{s.blurb}</span>
                  </span>
                  <ChevronIcon size={14} />
                </button>
              ))}
            </nav>
          )}
        </>
      )}
    </div>
  );

  const desktopDialog = (
    <div
      ref={surfaceRef}
      // The cabinet portals itself to <body> and covers the window, so the
      // dialog's own frame would only be an empty box glowing behind it.
      className={`settings-screen${circuit ? " ss-eclipsed" : ""}${closing ? " closing" : ""}`}
      data-zoom-pane="settings"
      role="dialog"
      aria-label="Settings"
      onClick={(e) => e.stopPropagation()}
    >
      {shell}
      {!circuit && (
        <>
          <header className="ss-head">
            <span className="ss-title">SETTINGS</span>
            <span className="ss-head-sub">{state.hello?.friendlyName ?? ""}</span>
            <button
              className="icon-btn ss-close"
              aria-label="Close settings"
              onClick={requestClose}
            >
              <XIcon size={15} />
            </button>
          </header>
          <div className="ss-body">
            <nav className="ss-nav" aria-label="Settings sections">
              {visibleSections.map((s) => (
                <button
                  key={s.id}
                  type="button"
                  className={`ss-nav-item${section === s.id ? " on" : ""}`}
                  aria-current={section === s.id}
                  onClick={() => setSection(s.id)}
                >
                  <span className="ss-nav-label">{s.label}</span>
                  <span className="ss-nav-sub">{s.blurb}</span>
                </button>
              ))}
            </nav>
            <div className="ss-content" key={section}>
              {paneContent}
            </div>
          </div>
        </>
      )}
    </div>
  );

  // Portaled to <body>: the opener lives inside the sidebar, whose mobile
  // off-canvas transform would otherwise drag this fixed overlay with it.
  return createPortal(
    <div
      className={
        `settings-screen-backdrop${isMobile ? " sheet" : ""}` +
        `${closing ? " closing" : ""}`
      }
      onClick={circuit || closing ? undefined : requestClose}
    >
      {isMobile ? mobileSheet : desktopDialog}
    </div>,
    document.body,
  );
}
