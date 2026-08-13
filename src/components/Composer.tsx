import type { ReactNode } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  Access,
  Agent,
  AgentInfo,
  Mode,
  OutgoingAttachment,
  Thread,
  ThreadSettings,
} from "../lib/protocol";
import { isAgentVisible } from "../lib/agentVisibility";
import { HERMES_HOME_PROJECT_ID, isQuickHomeProjectId } from "../lib/protocol";
import { effortForModel, resolveProjectView, useStore } from "../state/store";
import type { FeedItem } from "../state/feed";
import {
  AgentMark,
  ArrowUpIcon,
  BracketsIcon,
  CheckIcon,
  ChevronIcon,
  ChipIcon,
  EyeIcon,
  GaugeIcon,
  GlobeIcon,
  HammerIcon,
  MicIcon,
  PaperclipIcon,
  PencilIcon,
  PlusIcon,
  ShieldAlertIcon,
  StopIcon,
  WrenchIcon,
  XIcon,
} from "./icons";
import { COMPOSERPREFS_EVENT } from "../lib/appearance";
import { markJustSent } from "../lib/justSent";
import { ContextMeter, isRenderableUsage } from "./ContextMeter";
import { hermesPresence } from "./HermesPresence";

// Attachment limits — 8 files, 10 MB each.
const MAX_ATTACHMENTS = 8;
const MAX_ATTACHMENT_BYTES = 10 * 1024 * 1024;
// A turn is one WebSocket frame, and the server refuses a frame larger than
// MAX_WS_MESSAGE_BYTES (src-tauri/src/limits.rs) by closing the socket. Base64
// costs 4/3 of the file bytes, so the total is the ceiling that actually
// applies — the per-file limit above never was one for eight files at once.
// Checked here so too much at once is a sentence the user can read instead of a
// connection that drops mid-send.
const MAX_ATTACHMENT_TOTAL_BASE64 = 30 * 1024 * 1024;
const MAX_ATTACHMENT_TOTAL_LABEL = `${Math.floor((MAX_ATTACHMENT_TOTAL_BASE64 * 3) / 4 / (1024 * 1024))} MB`;
// Image types agents render inline (vision). Everything else is delivered as a
// workspace file the agent reads with its own tools — so we accept any file.
const IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"];
const ACCEPT_ATTR = "*/*";

function isImageType(mime: string): boolean {
  return mime.startsWith("image/");
}

/** Total wire cost of a draft's attachments (they travel as base64). */
function attachmentWireBytes(attachments: DraftAttachment[]): number {
  return attachments.reduce((total, a) => total + a.data.length, 0);
}

interface DraftAttachment {
  localId: string;
  name: string;
  mimeType: string;
  data: string; // base64, no data: prefix
}

interface NativeClipboardImage {
  name: string;
  mimeType: string;
  data: string;
}

let attachSeq = 0;

/** Read a File into a base64 DraftAttachment (rejects oversized/empty). */
function fileToAttachment(file: File): Promise<DraftAttachment | { error: string }> {
  return new Promise((resolve) => {
    const label = file.name || "file";
    if (file.size > MAX_ATTACHMENT_BYTES) {
      resolve({ error: `'${label}' exceeds the 10 MB limit.` });
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      const result = String(reader.result ?? "");
      const comma = result.indexOf(",");
      const mimeType = file.type || "application/octet-stream";
      resolve({
        localId: `a${++attachSeq}`,
        name: file.name || `pasted-image.${mimeType.split("/")[1] ?? "png"}`,
        mimeType,
        data: comma >= 0 ? result.slice(comma + 1) : result,
      });
    };
    reader.onerror = () => resolve({ error: `Couldn't read '${label}'.` });
    reader.readAsDataURL(file);
  });
}

/**
 * WebKit may put pasted images in `items` even when `files` is empty, so
 * `items` is a fallback only. Chromium populates both with the same image, and
 * each `getAsFile()` hands back a fresh File, so merging the two lists (no
 * reference-equality check can match) attaches every paste twice.
 */
function clipboardFiles(data: DataTransfer): File[] {
  const files = Array.from(data.files);
  if (files.length > 0) return files;
  const fromItems: File[] = [];
  for (const item of Array.from(data.items)) {
    if (item.kind !== "file") continue;
    const file = item.getAsFile();
    if (file) fromItems.push(file);
  }
  return fromItems;
}

/** Native file-manager clipboard entries plus the raw-image fallback. */
async function readNativeClipboardImages(): Promise<NativeClipboardImage[]> {
  if (!("__TAURI_INTERNALS__" in window)) return [];
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<NativeClipboardImage[]>("clipboard_images");
}

const EFFORT_LABELS: Record<string, string> = {
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "Extra High",
  max: "Max",
  ultra: "Ultra",
};

export function effortLabel(id: string): string {
  return EFFORT_LABELS[id] ?? id.charAt(0).toUpperCase() + id.slice(1);
}

/** Custom dropdown so agent options can carry their brand glyphs. */
export function AgentSelect({
  agents,
  value,
  disabled,
  onChange,
  direction = "up",
}: {
  agents: AgentInfo[];
  value: Agent;
  disabled: boolean;
  onChange: (agent: Agent) => void;
  direction?: "up" | "down";
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const current = agents.find((a) => a.id === value);

  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  return (
    <div className="agent-select" ref={ref}>
      <button
        type="button"
        className="agent-trigger"
        disabled={disabled}
        title={disabled ? "Wait for the current turn to finish" : "Choose agent — context carries over"}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <AgentMark agent={value} size={14} />
        <span>{current?.name ?? value}</span>
        {!disabled && <ChevronIcon size={11} open={open} className="row-chevron" />}
      </button>
      {open && (
        <div className={`agent-menu${direction === "down" ? " down" : ""}`} role="listbox">
          {agents.filter((a) => isAgentVisible(a.id)).map((a) => (
            <button
              type="button"
              key={a.id}
              role="option"
              aria-selected={a.id === value}
              className={`agent-option${a.id === value ? " on" : ""}`}
              onClick={() => {
                onChange(a.id);
                setOpen(false);
              }}
            >
              <AgentMark agent={a.id} size={14} />
              <span>{a.name}</span>
              {!a.available && <em className="agent-off">offline</em>}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** One selectable value inside a settings row's flyout. */
interface SettingOption {
  value: string;
  label: string;
  glyph?: ReactNode; // brand mark for agent options
  dot?: string; // hermes presence kind — a coloured status pip
  tag?: string; // trailing note ("offline")
  selected: boolean;
  onSelect: () => void;
}

/** One row of the consolidated composer settings menu. */
interface SettingRow {
  key: string;
  icon: ReactNode;
  label: string;
  valueLabel?: string;
  valueGlyph?: ReactNode;
  hint?: string;
  disabled?: boolean; // shown, but not openable (single/locked value)
  options: SettingOption[];
  action?: () => void; // a plain command row (e.g. attach files) — no flyout
}

/**
 * Every composer control folded into a single menu behind a plus button. Each
 * row shows the setting and its current value; hovering (desktop) or tapping
 * (touch) a row swings out a second-level flyout with that setting's choices.
 * One tidy surface in place of six naked <select>s.
 */
function ComposerSettings({ rows, isMobile }: { rows: SettingRow[]; isMobile: boolean }) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState<string | null>(null);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        setOpen(false);
        setActive(null);
      }
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        setOpen(false);
        setActive(null);
      }
    }
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className={`cset${isMobile ? " mobile" : ""}`} ref={ref}>
      <button
        type="button"
        className={`cset-trigger${open ? " on" : ""}`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label="Conversation settings"
        title="Conversation settings"
        onClick={() => {
          setOpen((v) => !v);
          setActive(null);
        }}
      >
        <PlusIcon size={19} strokeWidth={2.2} />
      </button>

      {open && (
        <div className="cset-menu" role="menu">
          {rows.map((row) => {
            const isActive = active === row.key;
            const isAction = !!row.action;
            return (
              <div
                key={row.key}
                className={`cset-row${isActive ? " active" : ""}${row.disabled ? " disabled" : ""}${isAction ? " cset-action" : ""}`}
                onMouseEnter={() => !isMobile && !row.disabled && !isAction && setActive(row.key)}
              >
                <button
                  type="button"
                  className="cset-row-btn"
                  role="menuitem"
                  aria-haspopup={row.disabled || isAction ? undefined : "menu"}
                  aria-expanded={isAction ? undefined : isActive}
                  disabled={row.disabled}
                  title={row.hint}
                  onClick={() => {
                    if (row.disabled) return;
                    if (isAction) {
                      row.action!();
                      setOpen(false);
                      setActive(null);
                      return;
                    }
                    setActive((k) => (k === row.key ? null : row.key));
                  }}
                >
                  <span className="cset-row-icon">{row.icon}</span>
                  <span className="cset-row-label">{row.label}</span>
                  {row.valueLabel !== undefined && (
                    <span className="cset-row-value">
                      {row.valueGlyph}
                      <span className="cset-row-value-text">{row.valueLabel}</span>
                    </span>
                  )}
                  {!row.disabled && !isAction && (
                    <ChevronIcon size={13} className="cset-row-caret" />
                  )}
                </button>

                {isActive && !row.disabled && !isAction && (
                  <div className="cset-sub" role="menu">
                    {row.options.map((opt) => (
                      <button
                        key={opt.value}
                        type="button"
                        role="menuitemradio"
                        aria-checked={opt.selected}
                        className={`cset-opt${opt.selected ? " on" : ""}`}
                        onClick={() => {
                          opt.onSelect();
                          setOpen(false);
                          setActive(null);
                        }}
                      >
                        {opt.glyph && <span className="cset-opt-glyph">{opt.glyph}</span>}
                        {opt.dot && (
                          <span className={`hermes-presence-inline ${opt.dot}`} aria-hidden="true" />
                        )}
                        <span className="cset-opt-label">{opt.label}</span>
                        {opt.tag && <em className="cset-opt-tag">{opt.tag}</em>}
                        {opt.selected && <CheckIcon size={14} className="cset-opt-check" />}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

interface PickerOption<T extends string> {
  id: T;
  icon: ReactNode;
  title: string;
  desc: string;
}

/**
 * A quiet inline pill dropdown for the two settings kept out on the bar (Mode,
 * Access). The trigger is a bare pill — icon only, or icon + short label —
 * that opens a menu of icon/title/description rows above the composer.
 */
function PillPicker<T extends string>({
  value,
  options,
  onChange,
  ariaLabel,
  showLabel = false,
}: {
  value: T;
  options: PickerOption<T>[];
  onChange: (id: T) => void;
  ariaLabel: string;
  showLabel?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const current = options.find((o) => o.id === value) ?? options[0];

  return (
    <div className="pill-picker" ref={ref}>
      <button
        type="button"
        className={`pill-trigger${open ? " on" : ""}${showLabel ? " labeled" : ""}`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={ariaLabel}
        title={showLabel ? ariaLabel : `${ariaLabel}: ${current.title}`}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="pill-trigger-icon">{current.icon}</span>
        {showLabel && <span className="pill-trigger-label">{current.title}</span>}
      </button>
      {open && (
        <div className="pill-menu" role="menu" aria-label={ariaLabel}>
          {options.map((o) => (
            <button
              key={o.id}
              type="button"
              role="menuitemradio"
              aria-checked={o.id === value}
              className={`pill-opt${o.id === value ? " on" : ""}`}
              onClick={() => {
                onChange(o.id);
                setOpen(false);
              }}
            >
              <span className="pill-opt-icon">{o.icon}</span>
              <span className="pill-opt-text">
                <span className="pill-opt-title">{o.title}</span>
                <span className="pill-opt-desc">{o.desc}</span>
              </span>
              {o.id === value && <CheckIcon size={15} className="pill-opt-check" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

// Icons sized to match the plus button (19), default stroke weight.
const MODE_OPTIONS: PickerOption<Mode>[] = [
  { id: "plan", icon: <HammerIcon size={19} />, title: "Plan", desc: "Chart the approach first — no file changes" },
  { id: "build", icon: <WrenchIcon size={19} />, title: "Build", desc: "Do the work and make the changes" },
];

const ACCESS_OPTIONS: PickerOption<Access>[] = [
  { id: "read", icon: <EyeIcon size={19} />, title: "Read", desc: "Read-only — look, never touch" },
  { id: "edits", icon: <PencilIcon size={19} />, title: "Edit", desc: "Edit files; asks before running" },
  { id: "full", icon: <ShieldAlertIcon size={19} />, title: "Full", desc: "Full access — runs commands freely" },
];

/**
 * Right-side pill (a picture of the current AI) that opens one menu for both
 * switching the AI and picking a model within it. Reuses SettingOption arrays so
 * the model-switch side effects (effort/context reset) stay in one place.
 */
function AgentModelPicker({
  agent,
  showAgents,
  agentsDisabled,
  agentOptions,
  modelGroupLabel,
  modelOptions,
  effortOptions,
}: {
  agent: Agent;
  showAgents: boolean;
  agentsDisabled: boolean;
  agentOptions: SettingOption[];
  modelGroupLabel: string;
  modelOptions: SettingOption[];
  effortOptions: SettingOption[];
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className="pill-picker ai-picker" ref={ref}>
      <button
        type="button"
        className={`pill-trigger ai-trigger${open ? " on" : ""}`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label="AI and model"
        title="AI & model"
        onClick={() => setOpen((v) => !v)}
      >
        <AgentMark agent={agent} size={20} />
      </button>
      {open && (
        <div className="pill-menu ai-menu" role="menu">
          {showAgents && (
            <>
              <div className="pill-menu-label">AI</div>
              {agentOptions.map((o) => (
                <button
                  key={o.value}
                  type="button"
                  role="menuitemradio"
                  aria-checked={o.selected}
                  disabled={agentsDisabled && !o.selected}
                  className={`pill-opt compact${o.selected ? " on" : ""}`}
                  title={agentsDisabled ? "Wait for the current turn to finish" : undefined}
                  onClick={() => o.onSelect()} // keep open so a model can follow
                >
                  <span className="pill-opt-icon">{o.glyph}</span>
                  <span className="pill-opt-text">
                    <span className="pill-opt-title">{o.label}</span>
                  </span>
                  {o.tag && <em className="cset-opt-tag">{o.tag}</em>}
                  {o.selected && <CheckIcon size={15} className="pill-opt-check" />}
                </button>
              ))}
              <div className="pill-menu-sep" />
            </>
          )}
          <div className="pill-menu-label">{modelGroupLabel}</div>
          {modelOptions.map((o) => (
            <button
              key={o.value}
              type="button"
              role="menuitemradio"
              aria-checked={o.selected}
              className={`pill-opt compact${o.selected ? " on" : ""}`}
              onClick={() => {
                o.onSelect();
                setOpen(false);
              }}
            >
              <span className="pill-opt-icon">
                {o.dot ? (
                  <span className={`hermes-presence-inline ${o.dot}`} aria-hidden="true" />
                ) : (
                  <ChipIcon size={15} />
                )}
              </span>
              <span className="pill-opt-text">
                <span className="pill-opt-title">{o.label}</span>
              </span>
              {o.tag && <em className="cset-opt-tag">{o.tag}</em>}
              {o.selected && <CheckIcon size={15} className="pill-opt-check" />}
            </button>
          ))}
          {effortOptions.length > 0 && (
            <>
              <div className="pill-menu-sep" />
              <div className="pill-menu-label">Effort</div>
              {effortOptions.map((o) => (
                <button
                  key={o.value}
                  type="button"
                  role="menuitemradio"
                  aria-checked={o.selected}
                  className={`pill-opt compact${o.selected ? " on" : ""}`}
                  onClick={() => {
                    o.onSelect();
                    setOpen(false);
                  }}
                >
                  <span className="pill-opt-icon">
                    <GaugeIcon size={15} />
                  </span>
                  <span className="pill-opt-text">
                    <span className="pill-opt-title">{o.label}</span>
                  </span>
                  {o.selected && <CheckIcon size={15} className="pill-opt-check" />}
                </button>
              ))}
            </>
          )}
        </div>
      )}
    </div>
  );
}

/** True on phone-width viewports (matches the CSS composer breakpoint). */
function useIsMobile(): boolean {
  const [mobile, setMobile] = useState(
    () => typeof window !== "undefined" && window.matchMedia("(max-width: 767px)").matches,
  );
  useEffect(() => {
    const mq = window.matchMedia("(max-width: 767px)");
    const on = () => setMobile(mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return mobile;
}

/**
 * Idle, opening the mic, capturing, or turning the clip into words. The last
 * two are split because only transcription is slow enough to need saying so.
 */
type MicState = "idle" | "starting" | "recording" | "transcribing";

interface SlashCommand {
  name: string;
  label: string;
  detail: string;
}

const SLASH_COMMANDS: SlashCommand[] = [
  {
    name: "/btw",
    label: "Add a note",
    detail: "Add context without interrupting the current turn",
  },
  {
    name: "/compact",
    label: "Compact context",
    detail: "Summarize the conversation to make room for more work",
  },
  {
    name: "/cost",
    label: "Show cost",
    detail: "Show the current session usage",
  },
  {
    name: "/help",
    label: "Show help",
    detail: "See the available agent commands",
  },
];

function slashContextAt(
  value: string,
  cursor: number,
): { start: number; query: string } | null {
  const beforeCursor = value.slice(0, cursor);
  const match = beforeCursor.match(/(?:^|\s)(\/[^\s]*)$/);
  if (!match) return null;
  const token = match[1];
  if (!/^\/[a-z0-9_-]*$/i.test(token)) return null;
  return { start: cursor - token.length, query: token.slice(1).toLowerCase() };
}

/** Matches the server's own cap; stop before it so the UI never hangs waiting. */
const MAX_DICTATION_SECONDS = 120;

function clock(seconds: number): string {
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

// One in-memory draft per thread/draft key so switching threads keeps text.
const textDrafts = new Map<string, string>();
const attachDrafts = new Map<string, DraftAttachment[]>();

interface ComposerProps {
  thread: Thread | null; // null when composing a brand-new thread
  quickMode?: "chat" | "build";
}

const CREATE_WORKSPACE_EVENT = "threadknot:create-workspace";

function BuildWorkspaceTray() {
  const { state, actions } = useStore();
  const [open, setOpen] = useState(false);
  const trayRef = useRef<HTMLDivElement>(null);
  const choices = useMemo(() => {
    const byId = new Map<string, { id: string; name: string; machineId?: string }>();

    for (const project of state.projects) {
      if (!isQuickHomeProjectId(project.id) && project.id !== HERMES_HOME_PROJECT_ID) {
        byId.set(project.id, { id: project.id, name: project.name });
      }
    }
    for (const workspace of state.workspaces) {
      for (const member of workspace.members) {
        const view = resolveProjectView(state, member.projectId);
        if (
          view &&
          !byId.has(view.project.id) &&
          !isQuickHomeProjectId(view.project.id) &&
          view.project.id !== HERMES_HOME_PROJECT_ID
        ) {
          byId.set(view.project.id, {
            id: view.project.id,
            name: view.project.name,
            machineId: view.machineId,
          });
        }
      }
    }
    return [...byId.values()];
  }, [state]);

  useEffect(() => {
    if (!open) return;
    function closeOnOutside(event: PointerEvent) {
      if (!trayRef.current?.contains(event.target as Node)) setOpen(false);
    }
    function closeOnEscape(event: KeyboardEvent) {
      if (event.key === "Escape") setOpen(false);
    }
    document.addEventListener("pointerdown", closeOnOutside);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutside);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [open]);

  return (
    <div className="build-workspace-tray" ref={trayRef}>
      <span className="build-workspace-label">Build in a workspace</span>
      <div className="build-workspace-actions">
        <button
          type="button"
          className="build-workspace-button primary"
          onClick={() => window.dispatchEvent(new Event(CREATE_WORKSPACE_EVENT))}
        >
          <PlusIcon size={14} />
          Create workspace
        </button>
        <span className="build-workspace-picker">
          <button
            type="button"
            className="build-workspace-button"
            aria-haspopup="menu"
            aria-expanded={open}
            onClick={() => setOpen((value) => !value)}
          >
            Select workspace
            <ChevronIcon open={open} size={13} />
          </button>
          {open && (
            <span className="build-workspace-menu" role="menu" aria-label="Select workspace">
              {choices.length > 0 ? (
                choices.map((choice) => (
                  <button
                    key={choice.id}
                    type="button"
                    role="menuitem"
                    onClick={() => {
                      setOpen(false);
                      actions.openDraft(choice.id, choice.machineId);
                    }}
                  >
                    {choice.name}
                  </button>
                ))
              ) : (
                <span className="build-workspace-empty">No workspaces yet</span>
              )}
            </span>
          )}
        </span>
      </div>
    </div>
  );
}

export function Composer({ thread, quickMode }: ComposerProps) {
  const { state, actions } = useStore();
  const draft = state.draft;
  const agents = state.hello?.agents ?? [];

  const agent: Agent = thread ? thread.agent : (draft?.agent ?? "claude");
  const settings: ThreadSettings | null = thread ? thread.settings : (draft?.settings ?? null);
  // Threads in the Hermes home have no folder for a coding agent to work in,
  // so they stay pinned to Hermes — the gateway picker below is the real choice.
  const hermesLocked =
    (thread ? thread.projectId : draft?.projectId) === HERMES_HOME_PROJECT_ID;
  const quickHome = isQuickHomeProjectId(
    thread ? thread.projectId : draft?.projectId,
  );
  const agentInfo: AgentInfo | undefined = agents.find((a) => a.id === agent);
  const models = agentInfo?.models ?? [];
  const currentModel = models.find((m) => m.id === settings?.model);
  const effortOptions = currentModel?.efforts ?? [];
  const usesProviderEffortDefault = agent === "claude";
  // The 200K/1M toggle is an Anthropic-model feature. Claudex profiles and
  // fixed-window Kimi aliases only report their upstream window.
  const supportsWideContext = agent === "claude" && !!currentModel?.supportsWideContext;
  const fixedContextWindow =
    agent === "claude" || agent === "claudex" || agent === "kimi"
      ? currentModel?.fixedContextWindow
      : undefined;
  const fixedContextLabel = fixedContextWindow
    ? fixedContextWindow >= 1_000_000
      ? `${fixedContextWindow / 1_000_000}M`
      : `${fixedContextWindow / 1_000}K`
    : null;

  // Absent on servers built before dictation existed, which hides the button.
  const dictation = state.hello?.dictation;

  const key = thread ? thread.id : `draft:${draft?.projectId ?? "?"}`;
  const [text, setText] = useState(() => textDrafts.get(key) ?? "");
  const [attachments, setAttachments] = useState<DraftAttachment[]>(
    () => attachDrafts.get(key) ?? [],
  );
  const [attachError, setAttachError] = useState<string | null>(null);
  const [fileDragActive, setFileDragActive] = useState(false);
  const isMobile = useIsMobile();
  const [mic, setMic] = useState<MicState>("idle");
  const [micSeconds, setMicSeconds] = useState(0);
  const [cursorPos, setCursorPos] = useState<number | null>(null);
  const [dismissedSlashKey, setDismissedSlashKey] = useState<string | null>(null);
  const [slashIndex, setSlashIndex] = useState(0);
  const taRef = useRef<HTMLTextAreaElement | null>(null);
  const composerInputRef = useRef<HTMLDivElement | null>(null);
  const taObserverRef = useRef<ResizeObserver | null>(null);
  const taWidthRef = useRef(0);
  const fileRef = useRef<HTMLInputElement>(null);
  const recordingRef = useRef<string | null>(null);

  useEffect(() => {
    setText(textDrafts.get(key) ?? "");
    setAttachments(attachDrafts.get(key) ?? []);
    setAttachError(null);
  }, [key]);

  // The one place the draft box is measured, so every trigger below lands the
  // same height for the same box. (220px is the .composer-card textarea
  // max-height; past it the box scrolls.)
  const resizeTextarea = useCallback(() => {
    const ta = taRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = `${Math.min(ta.scrollHeight, 220)}px`;
  }, []);

  useEffect(() => {
    resizeTextarea();
  }, [text, resizeTextarea]);

  // The height follows the font and the padding as much as the text, and the
  // composer preferences move both without touching a keystroke, which would
  // otherwise leave the inline height stale (a clipped or over-tall draft)
  // until the next character typed.
  useEffect(() => {
    window.addEventListener(COMPOSERPREFS_EVENT, resizeTextarea);
    return () => window.removeEventListener(COMPOSERPREFS_EVENT, resizeTextarea);
  }, [resizeTextarea]);

  // And the same staleness from the other direction: anything that rewraps the
  // draft by changing the box's WIDTH, whatever the cause: the width
  // preference, a pane divider drag, a window resize, the dictation badge.
  // Width only, deliberately: the routine writes a height, so reacting to
  // height would feed the observer its own output. Everything here calls that
  // one idempotent routine, so an event and an observation for the same change
  // simply compute the same height twice.
  const attachTextarea = useCallback(
    (node: HTMLTextAreaElement | null) => {
      taObserverRef.current?.disconnect();
      taObserverRef.current = null;
      taRef.current = node;
      if (!node) return;
      taWidthRef.current = 0; // the observer's first delivery seeds the real width
      const ro = new ResizeObserver((entries) => {
        const width = entries[0]?.contentRect.width ?? taWidthRef.current;
        if (width === taWidthRef.current) return;
        taWidthRef.current = width;
        resizeTextarea();
      });
      ro.observe(node);
      taObserverRef.current = ro;
    },
    [resizeTextarea],
  );

  useEffect(() => () => taObserverRef.current?.disconnect(), []);

  // Recording clock. The server caps the clip too, so hitting the cap here just
  // keeps the button honest about what already happened.
  useEffect(() => {
    if (mic !== "recording") return;
    const startedAt = Date.now();
    setMicSeconds(0);
    const timer = setInterval(() => {
      const elapsed = Math.floor((Date.now() - startedAt) / 1000);
      setMicSeconds(elapsed);
      if (elapsed >= MAX_DICTATION_SECONDS) void stopDictating();
    }, 250);
    return () => clearInterval(timer);
  }, [mic]);

  // Escape throws the clip away instead of transcribing it.
  useEffect(() => {
    if (mic !== "recording") return;
    function onKey(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      e.preventDefault();
      cancelDictating();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [mic]);

  // Leaving the composer (thread switch, pane close) must release the mic.
  useEffect(
    () => () => {
      const id = recordingRef.current;
      recordingRef.current = null;
      if (id) void actions.cancelDictation(id).catch(() => undefined);
    },
    [],
  );

  if (!settings) return null;

  // Waiting on the server either way, so the button can't be pressed again.
  const micBusy = mic === "starting" || mic === "transcribing";
  const running = thread ? thread.status !== "idle" : false;
  // Claude and Codex inject immediately. Kimi's ACP surface has no steer
  // method, so Threadknot accepts the same action and promotes it at the next
  // provider turn boundary without interrupting the active prompt.
  const acceptsRunningInput =
    agent === "claude" || agent === "claudex" || agent === "codex" || agent === "kimi";
  const canSend =
    (text.trim().length > 0 || attachments.length > 0) && state.conn === "online";
  // While a turn is live, the same primary control changes from Stop to Queue
  // as soon as there is a follow-up to send. Keep this text-only for now:
  // mid-turn attachments are not supported by the steer/queue protocol.
  const canQueue =
    running &&
    !!thread &&
    acceptsRunningInput &&
    text.trim().length > 0 &&
    state.conn === "online";

  const slashContext = slashContextAt(text, cursorPos ?? text.length);
  const slashCommands = SLASH_COMMANDS.filter(
    (command) => command.name.slice(1).startsWith(slashContext?.query ?? ""),
  );
  const slashKey = slashContext
    ? `${slashContext.start}:${slashContext.query}`
    : null;
  const slashOpen =
    slashContext != null && slashCommands.length > 0 && dismissedSlashKey !== slashKey;
  const selectedSlashIndex = Math.min(slashIndex, Math.max(0, slashCommands.length - 1));

  useEffect(() => {
    setSlashIndex(0);
  }, [slashKey]);

  // Latest renderable context snapshot for this thread. Dedicated snapshots
  // update during a turn/compaction/model switch; old turn-boundary usage stays
  // as a replay-compatible fallback. Usage-less turns never blank a good ring.
  const latestUsage =
    thread && state.feedThreadId === thread.id
      ? [...state.feed]
          .reverse()
          .find(
            (it): it is Extract<FeedItem, { type: "turn_end" | "context_usage" }> =>
              (it.type === "context_usage" || it.type === "turn_end") &&
              isRenderableUsage(it.usage),
          )?.usage
      : undefined;

  function updateText(v: string) {
    setText(v);
    textDrafts.set(key, v);
  }

  function trackCursor(next: HTMLTextAreaElement) {
    setCursorPos(next.selectionStart);
    setDismissedSlashKey(null);
  }

  function chooseSlashCommand(command: SlashCommand) {
    const currentCursor = cursorPos ?? text.length;
    const context = slashContextAt(text, currentCursor);
    if (!context) return;
    const insertion = `${command.name} `;
    const next = `${text.slice(0, context.start)}${insertion}${text.slice(currentCursor)}`;
    updateText(next);
    const nextCursor = context.start + insertion.length;
    setCursorPos(nextCursor);
    setDismissedSlashKey(null);
    requestAnimationFrame(() => {
      taRef.current?.focus();
      taRef.current?.setSelectionRange(nextCursor, nextCursor);
    });
  }

  function updateAttachments(next: DraftAttachment[]) {
    setAttachments(next);
    if (next.length > 0) attachDrafts.set(key, next);
    else attachDrafts.delete(key);
  }

  async function addFiles(files: File[]) {
    if (files.length === 0) return;
    setAttachError(null);
    const room = MAX_ATTACHMENTS - attachments.length;
    if (room <= 0) {
      setAttachError(`You can attach at most ${MAX_ATTACHMENTS} files.`);
      return;
    }
    const results = await Promise.all(files.slice(0, room).map(fileToAttachment));
    const added: DraftAttachment[] = [];
    for (const r of results) {
      if ("error" in r) setAttachError(r.error);
      else added.push(r);
    }
    if (files.length > room) {
      setAttachError(`You can attach at most ${MAX_ATTACHMENTS} files.`);
    }
    if (added.length === 0) return;
    const next = [...attachments, ...added];
    if (attachmentWireBytes(next) > MAX_ATTACHMENT_TOTAL_BASE64) {
      setAttachError(
        `That's too much to send in one message — attachments have to total under ${MAX_ATTACHMENT_TOTAL_LABEL}.`,
      );
      return;
    }
    updateAttachments(next);
  }

  async function pasteFromClipboard(data: DataTransfer): Promise<boolean> {
    const files = clipboardFiles(data);
    if (files.length > 0) {
      await addFiles(files);
      return true;
    }

    // GNOME Screenshot currently offers a raw image/png target with no path.
    // WebKitGTK can expose the MIME type but neither a File nor getAsFile(), so
    // ask the Tauri backend to read those bytes directly from Wayland.
    const advertisedImage = Array.from(data.types).some((type) =>
      IMAGE_TYPES.includes(type),
    );
    try {
      const images = await readNativeClipboardImages();
      if (images.length > 0) {
        setAttachError(null);
        const room = MAX_ATTACHMENTS - attachments.length;
        if (room <= 0) {
          setAttachError(`You can attach at most ${MAX_ATTACHMENTS} files.`);
          return true;
        }
        const next = [
          ...attachments,
          ...images.slice(0, room).map((image) => ({
            localId: `a${++attachSeq}`,
            name: image.name,
            mimeType: image.mimeType,
            data: image.data,
          })),
        ];
        if (attachmentWireBytes(next) > MAX_ATTACHMENT_TOTAL_BASE64) {
          setAttachError(
            `That's too much to send in one message — attachments have to total under ${MAX_ATTACHMENT_TOTAL_LABEL}.`,
          );
          return true;
        }
        updateAttachments(next);
        if (images.length > room) {
          setAttachError(`You can attach at most ${MAX_ATTACHMENTS} files.`);
        }
        return true;
      } else if (advertisedImage) {
        setAttachError("Couldn't read the image from the clipboard.");
      }
    } catch (error) {
      setAttachError(error instanceof Error ? error.message : String(error));
    }
    return false;
  }

  function insertPastedText(value: string, start: number, end: number) {
    if (!value) return;
    const next = `${text.slice(0, start)}${value}${text.slice(end)}`;
    updateText(next);
    requestAnimationFrame(() => {
      const cursor = start + value.length;
      taRef.current?.setSelectionRange(cursor, cursor);
    });
  }

  function removeAttachment(localId: string) {
    updateAttachments(attachments.filter((a) => a.localId !== localId));
  }

  /** Drop transcribed words in at the cursor without fusing them onto a word. */
  function insertDictated(spoken: string) {
    const ta = taRef.current;
    const start = ta?.selectionStart ?? text.length;
    const end = ta?.selectionEnd ?? text.length;
    const before = text.slice(0, start);
    const lead = before && !/\s$/.test(before) ? " " : "";
    updateText(`${before}${lead}${spoken}${text.slice(end)}`);
    requestAnimationFrame(() => {
      const cursor = start + lead.length + spoken.length;
      taRef.current?.focus();
      taRef.current?.setSelectionRange(cursor, cursor);
    });
  }

  async function startDictating() {
    setAttachError(null);
    setMic("starting");
    try {
      recordingRef.current = await actions.startDictation();
      setMic("recording");
    } catch (e) {
      recordingRef.current = null;
      setMic("idle");
      setAttachError(e instanceof Error ? e.message : String(e));
    }
  }

  async function stopDictating() {
    const id = recordingRef.current;
    if (!id) return;
    recordingRef.current = null;
    setMic("transcribing");
    try {
      const spoken = await actions.stopDictation(id);
      if (spoken) insertDictated(spoken);
      else setAttachError("Didn't catch that — nothing was said.");
    } catch (e) {
      setAttachError(e instanceof Error ? e.message : String(e));
    } finally {
      setMic("idle");
    }
  }

  function cancelDictating() {
    const id = recordingRef.current;
    recordingRef.current = null;
    setMic("idle");
    if (id) void actions.cancelDictation(id).catch(() => undefined);
  }

  // "/btw note" is muscle memory from Claude Code for mid-turn context; accept
  // (and strip) it in both states so it never leaks into the prompt verbatim.
  const stripBtw = (s: string) => s.replace(/^\/btw\b\s*/i, "");

  async function submit() {
    if (running && thread) {
      const note = stripBtw(text.trim());
      if (!note) return;
      if (!acceptsRunningInput) {
        setAttachError(
          "Messages while working aren't supported for this agent — press Stop to interrupt.",
        );
        return;
      }
      const typed = text;
      updateText("");
      setAttachError(null);
      markJustSent();
      try {
        await actions.steer(note);
      } catch {
        updateText(typed); // restore so nothing is lost
      }
      return;
    }
    const body = stripBtw(text.trim());
    if (!body && attachments.length === 0) return;
    const outgoing: OutgoingAttachment[] = attachments.map((a) => ({
      name: a.name,
      mimeType: a.mimeType,
      data: a.data,
    }));
    const keptAttachments = attachments;
    updateText("");
    updateAttachments([]);
    setAttachError(null);
    markJustSent();
    try {
      await actions.send(body, outgoing);
    } catch {
      updateText(body); // restore so nothing is lost
      updateAttachments(keptAttachments);
    }
  }

  async function runCompact() {
    if (running && thread) {
      if (!acceptsRunningInput) {
        setAttachError(
          "Messages while working aren't supported for this agent — press Stop to interrupt.",
        );
        return;
      }
      markJustSent();
      try {
        await actions.steer("/compact");
      } catch {
        setAttachError("Couldn't compact the context.");
      }
      return;
    }
    markJustSent();
    try {
      await actions.send("/compact", []);
    } catch {
      setAttachError("Couldn't compact the context.");
    }
  }

  function patch(p: Partial<ThreadSettings>) {
    void actions.setSettings({ ...settings!, ...p });
  }

  // AI + model now live in their own right-side pill (AgentModelPicker); these
  // option arrays feed it. Everything else folds into the plus-button menu.
  const isHermes = agent === "hermes";
  const agentOptions: SettingOption[] = agents
    .filter((a) => isAgentVisible(a.id))
    .map((a) => ({
      value: a.id,
      label: a.name,
      glyph: <AgentMark agent={a.id} size={17} />,
      tag: a.available ? undefined : "offline",
      selected: a.id === agent,
      onSelect: () =>
        thread ? void actions.setThreadAgent(a.id) : actions.setDraftAgent(a.id),
    }));

  const modelOptions: SettingOption[] = [];
  // A stored model the roster no longer advertises still names itself.
  if (!currentModel && settings.model) {
    modelOptions.push({
      value: settings.model,
      label: settings.model,
      selected: true,
      onSelect: () => undefined,
    });
  }
  for (const m of models) {
    const kind = isHermes ? hermesPresence(state.hermesStatuses[m.id]).kind : undefined;
    modelOptions.push({
      value: m.id,
      label: m.name,
      dot: kind,
      tag: kind === "offline" ? "offline" : kind === "checking" ? "checking" : undefined,
      selected: m.id === settings.model,
      onSelect: () =>
        patch({
          model: m.id,
          effort: effortForModel(m, settings.effort, usesProviderEffortDefault),
          wideContext:
            m.supportsWideContext && agent === "claude" ? settings.wideContext : undefined,
        }),
    });
  }
  const modelGroupLabel = isHermes ? "Gateway" : agent === "claudex" ? "Profile" : "Model";

  // Effort rides alongside model in the same AI pill — it belongs to the model.
  const effortOptionList: SettingOption[] = [];
  if (effortOptions.length > 0) {
    const effortValue =
      settings.effort && effortOptions.includes(settings.effort)
        ? settings.effort
        : usesProviderEffortDefault
          ? ""
          : (effortForModel(currentModel) ?? effortOptions[0]);
    if (usesProviderEffortDefault) {
      effortOptionList.push({
        value: "",
        label: `Default${currentModel?.defaultEffort ? ` (${effortLabel(currentModel.defaultEffort)})` : ""}`,
        selected: effortValue === "",
        onSelect: () => patch({ effort: undefined }),
      });
    }
    for (const id of effortOptions) {
      effortOptionList.push({
        value: id,
        label: effortLabel(id),
        selected: effortValue === id,
        onSelect: () => patch({ effort: id }),
      });
    }
  }

  // Everything else folds into one descriptor list; ComposerSettings renders it
  // as a plus-button menu with hover-out flyouts.
  const settingRows: SettingRow[] = [];

  if (agent === "claude" || ((agent === "claudex" || agent === "kimi") && fixedContextLabel)) {
    // Fixed-window models and 200K-only Claude models show the value but offer
    // no choice; only wide-context Claude gets a real toggle.
    const locked = !!fixedContextLabel || !supportsWideContext;
    settingRows.push({
      key: "ctx",
      icon: <BracketsIcon size={16} />,
      label: "Context",
      disabled: locked,
      valueLabel: fixedContextLabel
        ? fixedContextLabel
        : settings.wideContext && supportsWideContext
          ? "1M"
          : "200K",
      hint: fixedContextLabel
        ? `${currentModel?.name ?? "This model"} always uses a ${fixedContextLabel} context window`
        : supportsWideContext
          ? "Choose Claude's context-window size"
          : `${currentModel?.name ?? "This Claude model"} supports a 200K context window`,
      options: locked
        ? []
        : [
            {
              value: "200k",
              label: "200K",
              selected: !settings.wideContext,
              onSelect: () => patch({ wideContext: false }),
            },
            {
              value: "1m",
              label: "1M",
              selected: !!settings.wideContext,
              onSelect: () => patch({ wideContext: true }),
            },
          ],
    });
  }

  if (agent === "claude") {
    settingRows.push({
      key: "chrome",
      icon: <GlobeIcon size={16} />,
      label: "Chrome",
      valueLabel: settings.claudeChrome ? "Enabled" : "Default",
      disabled: running,
      hint: "Launch this Claude Code session with --chrome so it can use the Claude in Chrome extension",
      options: [
        {
          value: "default",
          label: "Default",
          selected: !settings.claudeChrome,
          onSelect: () => patch({ claudeChrome: false }),
        },
        {
          value: "enabled",
          label: "Enabled",
          selected: !!settings.claudeChrome,
          onSelect: () => patch({ claudeChrome: true }),
        },
      ],
    });
  }

  // Access and Mode stay out on the bar itself (see the inline segmented
  // controls below) rather than in the menu — they're the two settings changed
  // often enough to want one click, not two. Remote Hermes agents govern their
  // own approvals/plan mode server-side, so they get neither.

  // Attaching files is a one-shot command, not a setting, so it sits at the
  // foot of the same menu as its own row rather than a stray button.
  settingRows.push({
    key: "attach",
    icon: <PaperclipIcon size={16} />,
    label: "Attach files",
    disabled: running || attachments.length >= MAX_ATTACHMENTS,
    hint: "Attach images or files (or paste into the box)",
    options: [],
    action: () => fileRef.current?.click(),
  });

  // "Build with Opus 5…" / "Plan with Opus 5…", falling back to the mode word
  // alone when the model doesn't name itself.
  const verb = settings.mode === "plan" ? "Plan" : "Build";
  const workPlaceholder = currentModel?.name
    ? `${verb} with ${currentModel.name}…`
    : settings.mode === "plan"
      ? "Chart a course — describe what to plan…"
      : "Give your orders…";
  const placeholder =
    running && acceptsRunningInput
      ? agent === "kimi"
        ? "Queue a follow-up — Enter queues, Stop interrupts…"
        : "Queue a follow-up — Enter queues, Stop interrupts…"
      : agentInfo && !agentInfo.available
        ? (agentInfo.authHint ?? `${agentInfo.name} is not available`)
        : quickHome
          ? quickMode === "build"
            ? "Describe what you want to build…"
            : "Ask anything…"
          : workPlaceholder;

  return (
    <div className="composer">
      {agentInfo && !agentInfo.available && agentInfo.authHint && (
        <div className="composer-warn">{agentInfo.authHint}</div>
      )}
      {attachError && <div className="attach-err">{attachError}</div>}
      <div
        className={`composer-card${fileDragActive ? " file-drag" : ""}`}
        onDragEnter={(e) => {
          if (!Array.from(e.dataTransfer.types).includes("Files")) return;
          e.preventDefault();
          setFileDragActive(true);
        }}
        onDragOver={(e) => {
          if (!Array.from(e.dataTransfer.types).includes("Files")) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = "copy";
          setFileDragActive(true);
        }}
        onDragLeave={(e) => {
          if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
            setFileDragActive(false);
          }
        }}
        onDrop={(e) => {
          const files = Array.from(e.dataTransfer.files);
          if (files.length === 0) return;
          e.preventDefault();
          setFileDragActive(false);
          void addFiles(files);
        }}
      >
        {quickMode === "build" && <BuildWorkspaceTray />}
        {fileDragActive && <div className="composer-drop-hint">Drop files to attach</div>}
        {attachments.length > 0 && (
          <div className="composer-attachments">
            {attachments.map((a) => (
              <div
                className={isImageType(a.mimeType) ? "attach-chip" : "attach-chip attach-chip-file"}
                key={a.localId}
              >
                {isImageType(a.mimeType) ? (
                  <img src={`data:${a.mimeType};base64,${a.data}`} alt={a.name} />
                ) : (
                  <span className="attach-file-info">
                    <PaperclipIcon size={13} />
                    <span className="attach-file-name" title={a.name}>
                      {a.name}
                    </span>
                  </span>
                )}
                <button
                  type="button"
                  className="attach-remove"
                  title={`Remove ${a.name}`}
                  onClick={() => removeAttachment(a.localId)}
                >
                  <XIcon size={11} />
                </button>
              </div>
            ))}
          </div>
        )}
        <div
          ref={composerInputRef}
          className={`composer-input${mic === "transcribing" ? " busy" : ""}`}
        >
          {slashOpen && (
            <div className="slash-pop" role="listbox" aria-label="Commands">
              <div className="slash-pop-head">
                <span className="slash-pop-mark">/</span>
                <span>Commands</span>
                <span className="slash-pop-hint">↑↓ choose · Enter insert</span>
              </div>
              <div className="slash-pop-list">
                {slashCommands.map((command, index) => (
                  <button
                    key={command.name}
                    type="button"
                    role="option"
                    aria-selected={index === selectedSlashIndex}
                    className={`slash-option${index === selectedSlashIndex ? " selected" : ""}`}
                    onMouseDown={(event) => event.preventDefault()}
                    onMouseEnter={() => setSlashIndex(index)}
                    onClick={() => chooseSlashCommand(command)}
                  >
                    <span className="slash-option-name">{command.name}</span>
                    <span className="slash-option-copy">
                      <span className="slash-option-label">{command.label}</span>
                      <span className="slash-option-detail">{command.detail}</span>
                    </span>
                  </button>
                ))}
              </div>
            </div>
          )}
          {mic === "transcribing" && (
            <div className="dictation-busy" role="status">
              <span className="dictation-spinner" aria-hidden="true" />
              <span>Transcribing…</span>
            </div>
          )}
          <textarea
            ref={attachTextarea}
            rows={1}
            value={text}
            placeholder={placeholder}
            onChange={(e) => {
              updateText(e.target.value);
              trackCursor(e.currentTarget);
            }}
            onClick={(e) => trackCursor(e.currentTarget)}
            onSelect={(e) => trackCursor(e.currentTarget)}
            onPaste={(e) => {
              const files = clipboardFiles(e.clipboardData);
              if (files.length > 0) {
                e.preventDefault();
                void addFiles(files);
                return;
              }

              // Preserve ordinary browser text paste. In the desktop shell we
              // first check the native clipboard because WebKit can flatten a
              // copied file into indistinguishable plain path text.
              const pastedText = e.clipboardData.getData("text/plain");
              const hasText = pastedText.length > 0;
              const hasImageType = Array.from(e.clipboardData.types).some((type) =>
                IMAGE_TYPES.includes(type),
              );
              const nativeShell = "__TAURI_INTERNALS__" in window;
              if (hasText && !hasImageType && !nativeShell) return;

              // Native Linux file copies can look exactly like a plain path to
              // WebKit. Hold the default text insertion until the backend has
              // checked the native file-list clipboard, then restore normal text
              // paste when it wasn't an image file.
              e.preventDefault();
              const start = e.currentTarget.selectionStart;
              const end = e.currentTarget.selectionEnd;
              void pasteFromClipboard(e.clipboardData).then((attached) => {
                if (!attached && hasText) insertPastedText(pastedText, start, end);
              });
            }}
            onKeyDown={(e) => {
              if (slashOpen) {
                if (e.key === "ArrowDown") {
                  e.preventDefault();
                  setSlashIndex((index) => (index + 1) % slashCommands.length);
                  return;
                }
                if (e.key === "ArrowUp") {
                  e.preventDefault();
                  setSlashIndex(
                    (index) => (index - 1 + slashCommands.length) % slashCommands.length,
                  );
                  return;
                }
                if (e.key === "Escape") {
                  e.preventDefault();
                  setDismissedSlashKey(slashKey);
                  return;
                }
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  chooseSlashCommand(slashCommands[selectedSlashIndex]);
                  return;
                }
              }
              if (e.key === "Enter" && !e.shiftKey) {
                e.preventDefault();
                void submit();
              }
            }}
          />
        </div>
        <input
          ref={fileRef}
          type="file"
          accept={ACCEPT_ATTR}
          multiple
          hidden
          onChange={(e) => {
            void addFiles(Array.from(e.target.files ?? []));
            e.target.value = ""; // allow re-selecting the same file
          }}
        />
        <div className="composer-strip">
          <div className="composer-left">
            <ComposerSettings rows={settingRows} isMobile={isMobile} />
            {agent !== "hermes" && (
              <>
                <PillPicker
                  ariaLabel="Mode"
                  value={settings.mode}
                  options={MODE_OPTIONS}
                  onChange={(m) => patch({ mode: m })}
                  showLabel
                />
                {!quickHome && (
                  <PillPicker
                    ariaLabel="Access"
                    value={settings.access}
                    options={ACCESS_OPTIONS}
                    onChange={(a) => patch({ access: a })}
                    showLabel
                  />
                )}
              </>
            )}
          </div>

          <div className="composer-actions">
            {latestUsage && (
              <ContextMeter
                usage={latestUsage}
                anchorRef={composerInputRef}
                onCompact={() => void runCompact()}
              />
            )}
            {dictation?.available && (
              <button
                type="button"
                className={`mic-btn${mic === "recording" ? " recording" : micBusy ? " working" : ""}`}
                disabled={micBusy}
                aria-pressed={mic === "recording"}
                aria-label="Dictate"
                title={
                  mic === "recording"
                      ? "Stop and transcribe (Esc discards)"
                      : mic === "transcribing"
                        ? "Turning your words into text…"
                        : mic === "starting"
                          ? "Opening the microphone…"
                          : "Dictate: click to record, click again to transcribe"
                }
                onClick={() => (mic === "recording" ? void stopDictating() : void startDictating())}
              >
                <MicIcon size={18} />
                {mic === "recording" && <span className="mic-time">{clock(micSeconds)}</span>}
              </button>
            )}
            <AgentModelPicker
              agent={agent}
              showAgents={!hermesLocked}
              agentsDisabled={running}
              agentOptions={agentOptions}
              modelGroupLabel={modelGroupLabel}
              modelOptions={modelOptions}
              effortOptions={effortOptionList}
            />
            {running && !canQueue ? (
              // With an empty composer the primary button remains Stop. Once
              // the user types, it becomes the queue action below.
              <button
                type="button"
                className="send-btn stop"
                title="Interrupt turn"
                aria-label="Interrupt turn"
                onClick={() => void actions.interrupt().catch(() => undefined)}
              >
                <StopIcon size={16} />
              </button>
            ) : (
              <button
                type="button"
                className={`send-btn${running ? " queue" : ""}`}
                disabled={!canSend}
                title={running ? "Queue message (Enter)" : "Send (Enter)"}
                aria-label={running ? "Queue message" : "Send message"}
                onClick={() => void submit()}
              >
                <ArrowUpIcon size={19} />
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
