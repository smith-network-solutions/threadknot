import { memo, useEffect, useState } from "react";
import { createPortal } from "react-dom";
import type { FeedItem } from "../state/feed";
import { findThread, remoteMachineId, useStore } from "../state/store";
import {
  copyText,
  formatDuration,
  formatFullDateTime,
  formatMessageTime,
  formatTokens,
} from "../lib/format";
import { repoForPath } from "../lib/git";
import { artifactFileUrl, attachmentUrl } from "../lib/discovery";
import { downloadViaShell } from "../lib/download";
import { claimJustSent } from "../lib/justSent";
import { Markdown } from "./Markdown";
import { QuestionCard } from "./QuestionCard";
import {
  AgentMark,
  ArchiveIcon,
  CheckIcon,
  ChevronIcon,
  CopyIcon,
  DiffIcon,
  DownloadIcon,
  PopoutIcon,
  ShieldIcon,
  ToolGlyph,
  XIcon,
} from "./icons";
import {
  ArtifactPreview,
  artifactKind,
  artifactTypeLabel,
} from "./artifacts/ArtifactPreview";

import { AGENT_LABELS as AGENT_NAMES, threadParticipant, threadParticipants } from "../lib/protocol";

/**
 * The lane badge on an attributed message. Renders only when the thread
 * actually has more than one participant, so an ordinary single-agent chat
 * looks exactly as it did before Parley.
 */
function SpeakerChip({ speaker }: { speaker?: string }) {
  const { state } = useStore();
  const thread = state.feedThreadId ? findThread(state, state.feedThreadId) : undefined;
  if (!speaker || !thread || threadParticipants(thread).length < 2) return null;
  const lane = threadParticipant(thread, speaker);
  if (!lane) return null;
  return (
    <span className="speaker-chip" style={{ ["--lane-color" as string]: lane.color }}>
      <AgentMark agent={lane.agent} size={11} />
      {lane.name}
    </span>
  );
}

/** Small inline copy button; shows a check briefly after copying. */
export function CopyButton({
  value,
  label = "Copy",
  className = "copy-btn",
  stop = true,
}: {
  value: string;
  label?: string;
  className?: string;
  stop?: boolean;
}) {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      className={className}
      title={label}
      aria-label={label}
      onClick={async (e) => {
        if (stop) e.stopPropagation();
        if (await copyText(value)) {
          setDone(true);
          setTimeout(() => setDone(false), 1400);
        }
      }}
    >
      {done ? <CheckIcon size={13} /> : <CopyIcon size={13} />}
      {className.includes("copy-pill") && <span>{done ? "Copied" : "Copy"}</span>}
    </button>
  );
}

// ---- diff ---------------------------------------------------------------

export function DiffBody({ unified }: { unified: string }) {
  const lines = unified.replace(/\n$/, "").split("\n");
  return (
    <pre className="diff-body" data-no-pull>
      {lines.map((line, i) => {
        let cls = "diff-ctx";
        if (line.startsWith("+++") || line.startsWith("---")) cls = "diff-file";
        else if (line.startsWith("@@")) cls = "diff-hunk";
        else if (line.startsWith("+")) cls = "diff-add";
        else if (line.startsWith("-")) cls = "diff-del";
        return (
          <span key={i} className={`diff-line ${cls}`}>
            {line.length ? line : " "}
            {"\n"}
          </span>
        );
      })}
    </pre>
  );
}

function DiffCard({ item }: { item: Extract<FeedItem, { type: "diff" }> }) {
  const { state } = useStore();
  const [open, setOpen] = useState(true);
  const added = (item.unified.match(/^\+(?!\+\+)/gm) ?? []).length;
  const removed = (item.unified.match(/^-(?!--)/gm) ?? []).length;
  // Multi-repo projects: tag the diff with the repo that owns the file.
  const thread = state.activeThreadId ? findThread(state, state.activeThreadId) : null;
  const project = state.projects.find((p) => p.id === thread?.projectId);
  const repo = thread ? repoForPath(state.git[thread.projectId], project, item.path) : null;
  return (
    <div className="row-card diff-card">
      <div className="row-head diff-head">
        <button className="row-head-btn" onClick={() => setOpen(!open)}>
          <span className="row-glyph"><DiffIcon size={14} /></span>
          <span className="row-name">{item.path}</span>
          {repo && <span className="diff-repo">{repo.name}</span>}
          <span className="diff-stat">
            <em className="plus">+{added}</em> <em className="minus">−{removed}</em>
          </span>
          <ChevronIcon size={13} open={open} className="row-chevron" />
        </button>
        <CopyButton value={item.unified} label="Copy diff" />
      </div>
      {open && <DiffBody unified={item.unified} />}
    </div>
  );
}

// ---- artifact -----------------------------------------------------------

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/** A deliverable the agent produced this turn. Its durable snapshot is shown
 * right in the log; a larger viewer is portaled above the feed so conversation
 * zoom and narrow workspace panes cannot constrain it. */
function ArtifactCard({ item }: { item: Extract<FeedItem, { type: "artifact" }> }) {
  const { state, dispatch } = useStore();
  const thread = state.feedThreadId ? findThread(state, state.feedThreadId) : null;
  const [viewerOpen, setViewerOpen] = useState(false);
  const [downloadError, setDownloadError] = useState<string | null>(null);
  const machineId = remoteMachineId(state, thread?.machineId);
  const url = state.http ? artifactFileUrl(state.http, item.artifactId, { machineId }) : null;
  const kind = artifactKind({ ...item, id: item.artifactId });
  const typeLabel = artifactTypeLabel({ ...item, id: item.artifactId });

  const openArtifacts = () => {
    if (!thread) return;
    setViewerOpen(false);
    dispatch({ type: "workspace", projectId: thread.projectId, tab: "artifacts" });
    dispatch({ type: "artifactFocus", artifactId: item.artifactId });
  };

  const download = () => {
    if (!state.http) return;
    setDownloadError(null);
    const href = artifactFileUrl(state.http, item.artifactId, { download: true, machineId });
    const suggested = item.relPath.slice(item.relPath.lastIndexOf("/") + 1) || item.name;
    void downloadViaShell(href, suggested)
      .then((handled) => {
        if (handled) return;
        const anchor = document.createElement("a");
        anchor.href = href;
        anchor.download = suggested;
        anchor.click();
      })
      .catch((error) => setDownloadError(String((error as Error)?.message ?? error)));
  };

  useEffect(() => {
    if (!viewerOpen) return;
    const previous = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setViewerOpen(false);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      document.body.style.overflow = previous;
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [viewerOpen]);

  return (
    <>
      <article className={`artifact-card artifact-kind-${kind}`}>
        <div className="artifact-inline-preview">
          <ArtifactPreview artifact={{ ...item, id: item.artifactId }} url={url} mode="inline" />
          {kind !== "video" && (
            <button
              type="button"
              className="artifact-preview-hit"
              onClick={() => setViewerOpen(true)}
              aria-label={`Open ${item.name} in chat`}
              title="Open large preview"
            />
          )}
        </div>
        <div className="artifact-card-foot">
          <span className="artifact-type-badge" aria-hidden="true">{typeLabel}</span>
          <div className="artifact-card-copy">
            <strong title={item.name}>{item.name}</strong>
            <span>
              {item.description ?? (item.op === "modified" ? "Updated file" : "New file")}
              <i>·</i>{formatBytes(item.sizeBytes)}
            </span>
          </div>
          <button type="button" className="artifact-open-btn" onClick={() => setViewerOpen(true)}>
            Open
          </button>
        </div>
      </article>

      {viewerOpen && createPortal(
        <div
          className="artifact-lightbox-backdrop"
          onMouseDown={(event) => event.target === event.currentTarget && setViewerOpen(false)}
        >
          <section className="artifact-lightbox" role="dialog" aria-modal="true" aria-label={`Preview ${item.name}`}>
            <header className="artifact-lightbox-head">
              <button
                type="button"
                className="artifact-lightbox-close"
                onClick={() => setViewerOpen(false)}
                aria-label="Close preview"
                autoFocus
              >
                <XIcon size={17} />
              </button>
              <span className="artifact-type-badge">{typeLabel}</span>
              <div className="artifact-lightbox-title">
                <strong>{item.name}</strong>
                <span>{formatBytes(item.sizeBytes)}{item.description ? ` · ${item.description}` : ""}</span>
              </div>
              <div className="artifact-lightbox-actions">
                <button type="button" onClick={openArtifacts} title="Open in Artifacts">
                  <ArchiveIcon size={14} /><span>Artifacts</span>
                </button>
                <button type="button" onClick={download} disabled={!state.http} title="Download file">
                  <DownloadIcon size={14} /><span>Download</span>
                </button>
                <button type="button" onClick={() => setViewerOpen(false)} title="Close preview">
                  <XIcon size={15} /><span>Close</span>
                </button>
              </div>
            </header>
            {downloadError && <div className="artifact-lightbox-error">{downloadError}</div>}
            <div className="artifact-lightbox-body">
              <ArtifactPreview artifact={{ ...item, id: item.artifactId }} url={url} mode="full" />
            </div>
            <footer className="artifact-lightbox-mobile-actions">
              <button type="button" onClick={openArtifacts}><PopoutIcon size={15} /> Artifacts</button>
              <button type="button" onClick={download} disabled={!state.http}><DownloadIcon size={15} /> Download</button>
            </footer>
          </section>
        </div>,
        document.body,
      )}
    </>
  );
}

// ---- tool row -----------------------------------------------------------

const SUBAGENT_LINE_MAX = 300;

function SubagentCard({ item }: { item: Extract<FeedItem, { type: "tool" }> }) {
  const sub = item.subagent!;
  const running = sub.status === "running";
  const [open, setOpen] = useState(running);
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [running]);
  const started = sub.startedAt ? Date.parse(sub.startedAt) : NaN;
  const elapsed = running && Number.isFinite(started) ? formatDuration(now - started) : null;
  const lastActivity = sub.lastActivityAt ? Date.parse(sub.lastActivityAt) : NaN;
  const updateAge = running && Number.isFinite(lastActivity)
    ? formatDuration(Math.max(0, now - lastActivity))
    : null;
  const hasBody = running || sub.activity.length > 0 || !!sub.summary || !!sub.prompt;
  const title = sub.description || item.detail || sub.subagentType || "subagent";
  return (
    <div className={`row-card subagent-row status-${sub.status}`}>
      <button className="row-head" onClick={() => hasBody && setOpen(!open)} disabled={!hasBody}>
        <span className="row-glyph"><ToolGlyph name="Agent" /></span>
        <span className="row-name">{sub.background ? "Background agent" : "Subagent"}</span>
        {sub.subagentType && <span className="subagent-type">{sub.subagentType}</span>}
        <span className="row-detail">{title}</span>
        {elapsed && <span className="subagent-elapsed">{elapsed}</span>}
        {running ? (
          <span className="tool-spin" aria-label="running" />
        ) : sub.status === "error" ? (
          <span className="tool-flag">err</span>
        ) : (
          <span className="subagent-check" aria-label="done">✓</span>
        )}
        {hasBody && <ChevronIcon size={13} open={open} className="row-chevron" />}
      </button>
      {open && hasBody && (
        <div className="subagent-body">
          {running && (
            <div className="subagent-live-status">
              Running{elapsed ? ` · ${elapsed} elapsed` : ""}
              {updateAge
                ? ` · last child update ${updateAge} ago`
                : " · waiting for first child update"}
            </div>
          )}
          {sub.prompt && (
            <div className="subagent-line kind-prompt">
              <span className="subagent-line-tag">brief</span>
              <span className="subagent-line-text">{sub.prompt}</span>
            </div>
          )}
          {sub.activity.map((a, i) => (
            <div key={i} className={`subagent-line kind-${a.activity}`}>
              <span className="subagent-line-tag">{a.activity}</span>
              <span className="subagent-line-text">
                {a.text.length > SUBAGENT_LINE_MAX ? `${a.text.slice(0, SUBAGENT_LINE_MAX)}…` : a.text}
              </span>
            </div>
          ))}
          {sub.summary && (
            <div className="subagent-summary">
              <span className="subagent-line-tag">result</span>
              <span className="subagent-line-text">{sub.summary}</span>
            </div>
          )}
          {running && sub.activity.length === 0 && (
            <div className="subagent-waiting">
              Waiting for child activity. The elapsed timer means the call is
              still open; use Stop if it is taking unreasonably long.
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function ToolRow({ item }: { item: Extract<FeedItem, { type: "tool" }> }) {
  const { state, actions } = useStore();
  const [open, setOpen] = useState(false);
  // A replayed card carries an elided output; the full text is fetched the
  // first time someone actually opens it, so a long log stays cheap to load
  // without ever losing what the agent printed.
  const [full, setFull] = useState<string | null>(null);
  const [loadingFull, setLoadingFull] = useState(false);
  const threadId = state.feedThreadId;
  if (item.subagent) return <SubagentCard item={item} />;
  const hasDetail = item.detail.trim().length > 0;
  const hasOutput = item.output.length > 0;
  // A live tool is useful before it has printed anything: opening it reveals
  // the full invocation instead of leaving the truncated row disabled.
  const expandable = hasDetail || hasOutput || !item.done;
  const body = full ?? item.output;
  const expand = () => {
    const next = !open;
    setOpen(next);
    if (next && item.truncated && full === null && !loadingFull && threadId) {
      setLoadingFull(true);
      void actions
        .toolOutput(threadId, item.callId)
        .then((text) => text != null && setFull(text))
        .catch(() => undefined)
        .finally(() => setLoadingFull(false));
    }
  };
  return (
    <div className={`row-card tool-row${item.isError ? " is-error" : ""}${item.done ? "" : " is-live"}`}>
      <button
        className="row-head"
        onClick={() => expandable && expand()}
        disabled={!expandable}
        aria-expanded={expandable ? open : undefined}
      >
        <span className="row-glyph"><ToolGlyph name={item.name} /></span>
        <span className="row-name">{item.name}</span>
        {item.detail && <span className="row-detail">{item.detail}</span>}
        {!item.done && <span className="tool-spin" aria-label="running" />}
        {item.isError && <span className="tool-flag">err</span>}
        {expandable && <ChevronIcon size={13} open={open} className="row-chevron" />}
      </button>
      {open && expandable && (
        <div className="tool-body">
          {hasDetail && (
            <div className="tool-input-wrap">
              <div className="tool-section-label">call</div>
              <pre className="tool-input">{item.detail}</pre>
            </div>
          )}
          {body.length > 0 && (
            <div className="tool-output-wrap">
              <CopyButton value={body} label="Copy output" className="copy-btn floating" />
              <div className="tool-section-label">{item.done ? "output" : "live output"}</div>
              <pre className="tool-output">{body}</pre>
            </div>
          )}
          {!item.done && body.length === 0 && (
            <div className="tool-output-note">running · waiting for output…</div>
          )}
          {loadingFull && <div className="tool-output-note">loading full output…</div>}
        </div>
      )}
    </div>
  );
}

// ---- thinking -----------------------------------------------------------

function ThinkingBlock({ item }: { item: Extract<FeedItem, { type: "thinking" }> }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="thinking-block">
      <button className="thinking-head" onClick={() => setOpen(!open)}>
        <ChevronIcon size={12} open={open} className="row-chevron" />
        <span className={item.streaming ? "thinking-label pulsing" : "thinking-label"}>
          {item.streaming ? "Thinking…" : "Thought"}
        </span>
      </button>
      {open && (
        <div className="thinking-body">
          <Markdown text={item.text} />
        </div>
      )}
    </div>
  );
}

// ---- approval -----------------------------------------------------------

function ApprovalCard({ item }: { item: Extract<FeedItem, { type: "approval" }> }) {
  const { actions } = useStore();
  const [expanded, setExpanded] = useState(false);

  if (item.resolvedOptionId) {
    const chosen = item.options.find((o) => o.id === item.resolvedOptionId);
    const denied = chosen?.tone === "deny";
    return (
      <div className={`approval-resolved${denied ? " denied" : ""}`}>
        {denied ? <XIcon size={12} /> : <CheckIcon size={12} />}
        <span>
          {denied ? "Denied" : "Approved"} — {item.title}
          {chosen && chosen.tone !== "deny" ? ` (${chosen.label})` : ""}
        </span>
        <button className="ghost-link" onClick={() => setExpanded(!expanded)}>
          {expanded ? "hide" : "detail"}
        </button>
        {expanded && (
          <div className="approval-resolved-detail">
            <ApprovalDetail kind={item.approvalKind} detail={item.detail} />
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="approval-card">
      <div className="approval-head">
        <span className="approval-glyph"><ShieldIcon size={15} /></span>
        <span className="approval-kind">{item.approvalKind}</span>
        <span className="approval-title">{item.title}</span>
      </div>
      {item.detail && (
        <div className="approval-detail">
          <ApprovalDetail kind={item.approvalKind} detail={item.detail} />
        </div>
      )}
      <div className="approval-actions">
        {orderOptions(item.options).map((o) => (
          <button
            key={o.id}
            className={`btn tone-${o.tone}`}
            disabled={item.pending}
            onClick={() => actions.respondApproval(item.approvalId, o.id)}
          >
            {o.label}
          </button>
        ))}
      </div>
    </div>
  );
}

function orderOptions(options: Extract<FeedItem, { type: "approval" }>["options"]) {
  const rank = { allow: 0, allowAlways: 1, deny: 2 } as const;
  return [...options].sort((a, b) => rank[a.tone] - rank[b.tone]);
}

function ApprovalDetail({ kind, detail }: { kind: string; detail: string }) {
  if (kind === "exec" || kind === "tool") return <pre className="mono-block">{detail}</pre>;
  if (kind === "patch") return <DiffBody unified={detail} />;
  return <Markdown text={detail} />; // plan
}

// ---- turn divider / notes ----------------------------------------------

function TurnDivider({ item }: { item: Extract<FeedItem, { type: "turn_end" }> }) {
  const u = item.usage;
  const parts: string[] = [];
  if (item.durationMs != null) {
    parts.push(
      item.aborted
        ? `interrupted after ${formatDuration(item.durationMs)}`
        : `took ${formatDuration(item.durationMs)}`,
    );
  } else if (item.aborted) {
    parts.push("interrupted");
  }
  if (u) {
    const inp = formatTokens(u.inputTokens);
    const out = formatTokens(u.outputTokens);
    if (inp) parts.push(`${inp} in`);
    if (out) parts.push(`${out} out`);
    if (u.contextPct != null) parts.push(`${Math.round(u.contextPct)}% ctx`);
    if (u.costUsd != null) parts.push(`$${u.costUsd.toFixed(u.costUsd < 1 ? 3 : 2)}`);
  }
  return (
    <div className={`turn-divider${item.aborted ? " aborted" : ""}`}>
      <span className="turn-line" />
      {parts.length > 0 && <span className="turn-meta">{parts.join(" · ")}</span>}
      <span className="turn-line" />
    </div>
  );
}

/** Mid-thread handoff to a different provider/model (Traycer-style switch). */
function HandoffDivider({ item }: { item: Extract<FeedItem, { type: "turn" }> }) {
  if (!item.switched || !item.agent) return null;
  return (
    <div className="handoff-divider">
      <span className="turn-line" />
      <span className="handoff-chip">
        <AgentMark agent={item.agent} size={11} />
        handed off to {AGENT_NAMES[item.agent]}
        {item.model ? ` · ${item.model}` : ""}
      </span>
      <span className="turn-line" />
    </div>
  );
}

/**
 * A machine-issued brief handed to a lane (a Parley role prompt). It occupies
 * the user slot on the wire, but rendering it as a user bubble would read as
 * though the human wrote that wall of instructions — so it collapses to a
 * divider that can be expanded to audit exactly what the reviewer was told.
 */
function BriefDivider({ item }: { item: Extract<FeedItem, { type: "user" }> }) {
  const [open, setOpen] = useState(false);
  return (
    <div className={`brief-divider${open ? " open" : ""}`}>
      <div className="brief-head">
        <span className="turn-line" />
        <button
          className="brief-chip"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          title={open ? "Hide the brief" : "Show the brief this agent was given"}
        >
          <ShieldIcon size={11} />
          review requested
          <ChevronIcon size={11} open={open} />
        </button>
        <span className="turn-line" />
      </div>
      {open && <pre className="brief-body">{item.text}</pre>}
    </div>
  );
}

function MessageTime({
  timestamp,
  side,
}: {
  timestamp?: string;
  side: "user" | "assistant";
}) {
  if (!timestamp) return null;
  const full = formatFullDateTime(timestamp);
  return (
    <time
      className={`msg-time msg-time-${side}`}
      dateTime={timestamp}
      title={full}
      aria-label={`Sent ${full}`}
    >
      {formatMessageTime(timestamp)}
    </time>
  );
}

function MessageImage({ url, name }: { url: string; name: string }) {
  const [failed, setFailed] = useState(false);
  return (
    <a
      className={`msg-user-image${failed ? " failed" : ""}`}
      href={url}
      target="_blank"
      rel="noreferrer"
      aria-label={`Open ${name}`}
      title={`Open ${name}`}
    >
      {failed ? (
        <span className="msg-user-image-fallback">
          <strong>Preview unavailable</strong>
          <small>{name}</small>
        </span>
      ) : (
        <img src={url} alt={name} loading="lazy" onError={() => setFailed(true)} />
      )}
    </a>
  );
}

function UserMessage({ item }: { item: Extract<FeedItem, { type: "user" }> }) {
  const { state } = useStore();
  const threadId = state.feedThreadId;
  const images = (item.attachments ?? []).filter((a) => a.mimeType.startsWith("image/"));
  const hasText = item.text.trim().length > 0;
  // Claimed once, at first render: this bubble is the echo of a send from this
  // client, so it gets the arcade slam entrance. Scrollback and messages from
  // other devices find the beacon empty and mount silently.
  const [justSent] = useState(claimJustSent);
  return (
    <div className={`msg-user-wrap${justSent ? " just-sent" : ""}`}>
      {item.midTurn && <div className="msg-user-midturn">added while working</div>}
      <div
        className={`msg-user${images.length > 0 ? " has-attachments" : ""}${!hasText ? " image-only" : ""}`}
      >
        {hasText && <div className="msg-user-text">{item.text}</div>}
        {state.http && threadId && images.length > 0 && (
          <div className={`msg-user-attachments ${images.length === 1 ? "single" : "gallery"}`}>
            {images.map((a) => {
              const url = attachmentUrl(state.http!, threadId, a.id, {
                machineId: remoteMachineId(state, findThread(state, threadId)?.machineId),
              });
              return <MessageImage key={a.id} url={url} name={a.name} />;
            })}
          </div>
        )}
      </div>
      <MessageTime timestamp={item.timestamp} side="user" />
    </div>
  );
}

// ---- dispatcher ---------------------------------------------------------

export const FeedItemView = memo(function FeedItemView({ item }: { item: FeedItem }) {
  switch (item.type) {
    case "user":
      // A role brief is not the user talking — see BriefDivider.
      return item.injected ? <BriefDivider item={item} /> : <UserMessage item={item} />;
    case "assistant":
      return (
        <div className={`msg-assistant${item.streaming ? " streaming" : ""}`}>
          <SpeakerChip speaker={item.speaker} />
          <Markdown text={item.text} />
          {item.streaming && <span className="stream-caret" />}
          {!item.streaming && item.text.length > 0 && (
            <CopyButton value={item.text} label="Copy message" className="copy-btn msg-copy" />
          )}
          {!item.streaming && (
            <MessageTime timestamp={item.timestamp} side="assistant" />
          )}
        </div>
      );
    case "thinking":
      return <ThinkingBlock item={item} />;
    case "tool":
      return <ToolRow item={item} />;
    case "diff":
      return <DiffCard item={item} />;
    case "artifact":
      return <ArtifactCard item={item} />;
    case "approval":
      return <ApprovalCard item={item} />;
    case "question":
      return <QuestionCard item={item} />;
    case "turn_end":
      return <TurnDivider item={item} />;
    case "context_usage":
      return null;
    case "turn":
      return <HandoffDivider item={item} />;
    case "note":
      return (
        <div className={`feed-note note-${item.noteKind}`}>
          {item.noteKind === "error" ? `⚠ ${item.text}` : item.text}
        </div>
      );
    default:
      return null;
  }
});
