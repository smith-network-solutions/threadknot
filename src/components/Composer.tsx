import { useCallback, useEffect, useRef, useState } from "react";
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
import { HERMES_HOME_PROJECT_ID } from "../lib/protocol";
import { effortForModel, useStore } from "../state/store";
import type { FeedItem } from "../state/feed";
import {
  AgentMark,
  ArrowUpIcon,
  ChevronIcon,
  MicIcon,
  PaperclipIcon,
  StopIcon,
  XIcon,
} from "./icons";
import { COMPOSERPREFS_EVENT } from "../lib/appearance";
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

const ACCESS: { id: Access; label: string }[] = [
  { id: "read", label: "Read-only" },
  { id: "edits", label: "Edits allowed" },
  { id: "full", label: "Full access" },
];

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
}

export function Composer({ thread }: ComposerProps) {
  const { state, actions } = useStore();
  const draft = state.draft;
  const agents = state.hello?.agents ?? [];

  const agent: Agent = thread ? thread.agent : (draft?.agent ?? "claude");
  const settings: ThreadSettings | null = thread ? thread.settings : (draft?.settings ?? null);
  // Threads in the Hermes home have no folder for a coding agent to work in,
  // so they stay pinned to Hermes — the gateway picker below is the real choice.
  const hermesLocked =
    (thread ? thread.projectId : draft?.projectId) === HERMES_HOME_PROJECT_ID;
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
  const [optionsOpen, setOptionsOpen] = useState(false);
  const [mic, setMic] = useState<MicState>("idle");
  const [micSeconds, setMicSeconds] = useState(0);
  const taRef = useRef<HTMLTextAreaElement | null>(null);
  const taObserverRef = useRef<ResizeObserver | null>(null);
  const taWidthRef = useRef(0);
  const fileRef = useRef<HTMLInputElement>(null);
  const optionsRef = useRef<HTMLDivElement>(null);
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
    try {
      await actions.send(body, outgoing);
    } catch {
      updateText(body); // restore so nothing is lost
      updateAttachments(keptAttachments);
    }
  }

  function patch(p: Partial<ThreadSettings>) {
    void actions.setSettings({ ...settings!, ...p });
  }

  // Close the mobile options sheet when tapping outside it.
  useEffect(() => {
    if (!optionsOpen) return;
    function onDown(e: MouseEvent) {
      if (optionsRef.current && !optionsRef.current.contains(e.target as Node)) {
        setOptionsOpen(false);
      }
    }
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [optionsOpen]);

  // Collapse the sheet automatically when leaving mobile width.
  useEffect(() => {
    if (!isMobile) setOptionsOpen(false);
  }, [isMobile]);

  // The settings controls — rendered inline on desktop, and inside the
  // collapsible options sheet on mobile so the bar isn't cluttered.
  const controls = (
    <>
      {!hermesLocked && (
        <div className="ctl">
          <span className="ctl-label">Agent</span>
          <AgentSelect
            agents={agents}
            value={agent}
            disabled={running}
            onChange={(a) =>
              thread ? void actions.setThreadAgent(a) : actions.setDraftAgent(a)
            }
          />
        </div>
      )}

      <label className="ctl">
        <span className="ctl-label">
          {agent === "hermes" ? "Hermes agent" : agent === "claudex" ? "Profile" : "Model"}
          {/* Live presence of the selected gateway; the options carry a text
              tag for the others. Offline gateways are never disabled (one may
              come back mid-chat), only marked. */}
          {agent === "hermes" &&
            settings.model &&
            (() => {
              const p = hermesPresence(state.hermesStatuses[settings.model]);
              return (
                <span className={`hermes-presence-inline ctl-presence ${p.kind}`} title={p.title} />
              );
            })()}
        </span>
        <select
          value={settings.model}
          onChange={(e) => {
            const m = models.find((x) => x.id === e.target.value);
            patch({
              model: e.target.value,
              effort: effortForModel(m, settings.effort, usesProviderEffortDefault),
              wideContext:
                m?.supportsWideContext && agent === "claude"
                  ? settings.wideContext
                  : undefined,
            });
          }}
        >
          {!currentModel && settings.model && (
            <option value={settings.model}>{settings.model}</option>
          )}
          {models.map((m) => {
            // For hermes gateways, tag offline/checking ones inline (native
            // <option>s can't host the colored dot the label carries).
            const tag =
              agent === "hermes"
                ? (() => {
                    const kind = hermesPresence(state.hermesStatuses[m.id]).kind;
                    return kind === "offline" ? " · offline" : kind === "checking" ? " · checking" : "";
                  })()
                : "";
            return (
              <option key={m.id} value={m.id}>
                {m.name}
                {tag}
              </option>
            );
          })}
        </select>
      </label>

      {effortOptions.length > 0 && (
        <label className="ctl">
          <span className="ctl-label">Effort</span>
          <select
            value={
              settings.effort && effortOptions.includes(settings.effort)
                ? settings.effort
                : usesProviderEffortDefault
                  ? ""
                  : (effortForModel(currentModel) ?? effortOptions[0])
            }
            onChange={(e) => patch({ effort: e.target.value || undefined })}
          >
            {usesProviderEffortDefault && (
              <option value="">
                Default
                {currentModel?.defaultEffort
                  ? ` (${effortLabel(currentModel.defaultEffort)})`
                  : ""}
              </option>
            )}
            {effortOptions.map((id) => (
              <option key={id} value={id}>
                {effortLabel(id)}
              </option>
            ))}
          </select>
        </label>
      )}

      {(agent === "claude" ||
        ((agent === "claudex" || agent === "kimi") && fixedContextLabel)) && (
        <label
          className="ctl"
          title={
            fixedContextLabel
              ? `${currentModel?.name ?? "This model"} always uses a ${fixedContextLabel} context window`
              : supportsWideContext
              ? "Choose Claude's context-window size"
              : `${currentModel?.name ?? "This Claude model"} supports a 200K context window`
          }
        >
          <span className="ctl-label">Context window</span>
          <select
            aria-label="Context window"
            value={
              fixedContextLabel
                ? "fixed"
                : settings.wideContext && supportsWideContext
                  ? "1m"
                  : "200k"
            }
            disabled={!!fixedContextLabel || !supportsWideContext}
            onChange={(e) => patch({ wideContext: e.target.value === "1m" })}
          >
            {fixedContextLabel ? (
              <option value="fixed">{fixedContextLabel} · model default</option>
            ) : (
              <>
                <option value="200k">
                  {supportsWideContext ? "200K" : "200K · model limit"}
                </option>
                {supportsWideContext && <option value="1m">1M</option>}
              </>
            )}
          </select>
        </label>
      )}

      {agent === "claude" && (
        <label
          className="ctl"
          title="Launch this Claude Code session with --chrome so it can use the Claude in Chrome extension"
        >
          <span className="ctl-label">Claude in Chrome</span>
          <select
            aria-label="Claude in Chrome"
            value={settings.claudeChrome ? "enabled" : "default"}
            disabled={running}
            onChange={(e) => patch({ claudeChrome: e.target.value === "enabled" })}
          >
            <option value="default">Default</option>
            <option value="enabled">Enabled</option>
          </select>
        </label>
      )}

      {/* Remote Hermes agents govern their own approvals/tools server-side —
          Threadknot's access levels and plan mode don't apply to them. */}
      {agent !== "hermes" && (
        <label className="ctl">
          <span className="ctl-label">Access</span>
          <select
            value={settings.access}
            onChange={(e) => patch({ access: e.target.value as Access })}
          >
            {ACCESS.map((x) => (
              <option key={x.id} value={x.id}>
                {x.label}
              </option>
            ))}
          </select>
        </label>
      )}

      {agent !== "hermes" && (
        <div className="ctl">
          <span className="ctl-label">Mode</span>
          <div className="seg" role="group" aria-label="Mode">
            {(["plan", "build"] as Mode[]).map((m) => (
              <button
                key={m}
                className={settings.mode === m ? "seg-btn on" : "seg-btn"}
                onClick={() => patch({ mode: m })}
              >
                {m === "plan" ? "Plan" : "Build"}
              </button>
            ))}
          </div>
        </div>
      )}
    </>
  );

  const placeholder =
    running && acceptsRunningInput
      ? agent === "kimi"
        ? "Queue a follow-up — Enter sends, Stop interrupts…"
        : "Add context to the running turn — Enter sends, Stop interrupts…"
      : settings.mode === "plan"
        ? "Chart a course — describe what to plan…"
        : agentInfo && !agentInfo.available
          ? (agentInfo.authHint ?? `${agentInfo.name} is not available`)
          : "Give your orders…";

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
        <div className={`composer-input${mic === "transcribing" ? " busy" : ""}`}>
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
            onChange={(e) => updateText(e.target.value)}
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
          {!isMobile && <div className="composer-controls">{controls}</div>}
          {isMobile && optionsOpen && (
            <div className="composer-options" ref={optionsRef}>
              {controls}
            </div>
          )}

          <div className="composer-actions">
            {isMobile && (
              <button
                type="button"
                className={`opt-btn${optionsOpen ? " on" : ""}`}
                aria-expanded={optionsOpen}
                aria-label="Composer options"
                onClick={() => setOptionsOpen((o) => !o)}
              >
                <AgentMark agent={agent} size={15} />
                <span className="opt-label">
                  {currentModel?.name ?? agentInfo?.name ?? agent}
                </span>
                <ChevronIcon size={10} open={optionsOpen} />
              </button>
            )}
            {latestUsage && <ContextMeter usage={latestUsage} />}
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
                <MicIcon size={17} />
                {mic === "recording" && <span className="mic-time">{clock(micSeconds)}</span>}
              </button>
            )}
            <button
              type="button"
              className="attach-btn"
              disabled={running || attachments.length >= MAX_ATTACHMENTS}
              title="Attach images (or paste from clipboard)"
              onClick={() => fileRef.current?.click()}
            >
              <PaperclipIcon size={17} />
            </button>
            {running && (
              <button
                className="send-btn stop"
                title="Interrupt turn"
                aria-label="Interrupt turn"
                onClick={() => void actions.interrupt().catch(() => undefined)}
              >
                <StopIcon size={16} />
              </button>
            )}
            <button
              className="send-btn"
              disabled={!canSend || (running && !acceptsRunningInput)}
              title={
                running
                  ? agent === "kimi"
                    ? "Queue follow-up (Enter)"
                    : "Add to running turn (Enter)"
                  : "Send (Enter)"
              }
              onClick={() => void submit()}
            >
              <ArrowUpIcon size={17} />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
