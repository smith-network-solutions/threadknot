import { memo, useEffect, useState, type Dispatch, type ReactNode } from "react";
import { createPortal } from "react-dom";
import type { FeedItem } from "../state/feed";
import type { Action, AppState, ThreadknotActions } from "../state/store";
import type { GitRepoInfo, Participant, Project } from "../lib/protocol";
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
import { parseReply, replyPreview, type ReplyTarget } from "../lib/reply";
import { isImageAttachment } from "../lib/attachments";
import {
  AgentMark,
  ArchiveIcon,
  ArrowDownIcon,
  ArrowUpIcon,
  BracketsIcon,
  BrainIcon,
  CheckIcon,
  ClockIcon,
  ChevronIcon,
  CopyIcon,
  DiffIcon,
  DollarIcon,
  DownloadIcon,
  PopoutIcon,
  ReplyIcon,
  ShieldIcon,
  ToolGlyph,
  XIcon,
} from "./icons";
import {
  ArtifactPreview,
  artifactKind,
  artifactTypeLabel,
} from "./artifacts/ArtifactPreview";

import { AGENT_LABELS as AGENT_NAMES } from "../lib/protocol";

type FeedActions = Pick<
  ThreadknotActions,
  "toolOutput" | "respondApproval" | "setQuestionAnswers" | "respondQuestion"
>;

/** Stable, non-feed state needed by an individual row. Keeping this out of the
 * global StoreContext lets memoized historical rows stay asleep while tokens
 * arrive for the one live assistant message. */
export interface FeedRenderContext {
  threadId: string | null;
  projectId: string | null;
  machineId?: string;
  http: AppState["http"];
  project?: Project;
  gitRepos?: GitRepoInfo[];
  participants: Participant[];
  dispatch: Dispatch<Action>;
  actions: FeedActions;
  onReply: (target: ReplyTarget) => void;
  onOpenFile: (path: string) => void;
}

/** The provider identity badge on every assistant message. */
function SpeakerChip({
  speaker,
  participants,
}: {
  speaker?: string;
  participants: Participant[];
}) {
  // New events carry their lane id. Older single-agent logs do not, so use the
  // synthesized builder lane as a safe fallback for those messages.
  const lane = participants.find((participant) => participant.id === speaker) ??
    (!speaker ? participants[0] : undefined);
  if (!lane) return null;
  return (
    <span
      className="speaker-chip"
      style={{ ["--lane-color" as string]: lane.color }}
      aria-label={`${lane.name} agent`}
    >
      <span className="speaker-chip-mark">
        <AgentMark agent={lane.agent} size={12} />
      </span>
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
      {done ? (
        <CheckIcon size={className.includes("msg-copy") ? 15 : 13} />
      ) : (
        <CopyIcon size={className.includes("msg-copy") ? 15 : 13} />
      )}
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

function DiffCard({
  item,
  render,
}: {
  item: Extract<FeedItem, { type: "diff" }>;
  render: FeedRenderContext;
}) {
  const [open, setOpen] = useState(true);
  const added = (item.unified.match(/^\+(?!\+\+)/gm) ?? []).length;
  const removed = (item.unified.match(/^-(?!--)/gm) ?? []).length;
  // Multi-repo projects: tag the diff with the repo that owns the file.
  const repo = repoForPath(render.gitRepos, render.project, item.path);
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
function ArtifactCard({
  item,
  render,
}: {
  item: Extract<FeedItem, { type: "artifact" }>;
  render: FeedRenderContext;
}) {
  const [viewerOpen, setViewerOpen] = useState(false);
  const [downloadError, setDownloadError] = useState<string | null>(null);
  const machineId = render.machineId;
  const url = render.http ? artifactFileUrl(render.http, item.artifactId, { machineId }) : null;
  const kind = artifactKind({ ...item, id: item.artifactId });
  const typeLabel = artifactTypeLabel({ ...item, id: item.artifactId });

  const openArtifacts = () => {
    if (!render.projectId) return;
    setViewerOpen(false);
    render.dispatch({ type: "workspace", projectId: render.projectId, tab: "artifacts" });
    render.dispatch({ type: "artifactFocus", artifactId: item.artifactId });
  };

  const download = () => {
    if (!render.http) return;
    setDownloadError(null);
    const href = artifactFileUrl(render.http, item.artifactId, { download: true, machineId });
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
                  <button type="button" onClick={download} disabled={!render.http} title="Download file">
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
              <button type="button" onClick={download} disabled={!render.http}><DownloadIcon size={15} /> Download</button>
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

function ToolRow({
  item,
  render,
}: {
  item: Extract<FeedItem, { type: "tool" }>;
  render: FeedRenderContext;
}) {
  const [open, setOpen] = useState(false);
  // A replayed card carries an elided output; the full text is fetched the
  // first time someone actually opens it, so a long log stays cheap to load
  // without ever losing what the agent printed.
  const [full, setFull] = useState<string | null>(null);
  const [loadingFull, setLoadingFull] = useState(false);
  const threadId = render.threadId;
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
      void render.actions
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
          <Markdown text={item.text} streaming={item.streaming} />
        </div>
      )}
    </div>
  );
}

// ---- approval -----------------------------------------------------------

function ApprovalCard({
  item,
  render,
}: {
  item: Extract<FeedItem, { type: "approval" }>;
  render: FeedRenderContext;
}) {
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
            onClick={() => render.actions.respondApproval(item.approvalId, o.id)}
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
  const metrics: { key: string; label: string; value: string; icon: ReactNode }[] = [];
  if (item.durationMs != null) {
    metrics.push({
      key: "duration",
      label: item.aborted ? "Interrupted after" : "Duration",
      value: item.aborted
        ? `after ${formatDuration(item.durationMs)}`
        : formatDuration(item.durationMs),
      icon: <ClockIcon size={12} />,
    });
  } else if (item.aborted) {
    metrics.push({
      key: "duration",
      label: "Interrupted",
      value: "interrupted",
      icon: <ClockIcon size={12} />,
    });
  }
  if (u) {
    const inp = formatTokens(u.inputTokens);
    const out = formatTokens(u.outputTokens);
    if (inp) {
      metrics.push({
        key: "input",
        label: "Input tokens",
        value: inp,
        icon: <ArrowDownIcon size={12} />,
      });
    }
    if (out) {
      metrics.push({
        key: "output",
        label: "Output tokens",
        value: out,
        icon: <ArrowUpIcon size={12} />,
      });
    }
    if (u.contextPct != null) {
      metrics.push({
        key: "context",
        label: "Context used",
        value: `${Math.round(u.contextPct)}%`,
        icon: <BracketsIcon size={12} />,
      });
    }
    if (u.costUsd != null) {
      metrics.push({
        key: "cost",
        label: "Estimated cost",
        value: `$${u.costUsd.toFixed(u.costUsd < 1 ? 3 : 2)}`,
        icon: <DollarIcon size={12} />,
      });
    }
  }
  return (
    <div className={`turn-divider stats-only${item.aborted ? " aborted" : ""}`}>
      {metrics.length > 0 && (
        <span className="turn-meta" aria-label="Turn details">
          {metrics.map((metric) => (
            <span className="turn-stat" key={metric.key} title={metric.label} aria-label={`${metric.label}: ${metric.value}`}>
              {metric.icon}
              <span>{metric.value}</span>
            </span>
          ))}
        </span>
      )}
    </div>
  );
}

/** Calculate thought time for the first assistant item in each turn in one
 * forward pass. This used to scan the whole feed once per assistant row. */
export function thoughtTimesForFeed(feed: FeedItem[]): Record<string, string> {
  const out: Record<string, string> = {};
  let turnStarted: number | null = null;
  let sawAssistant = false;
  for (const item of feed) {
    if (item.type === "turn_end") {
      turnStarted = null;
      sawAssistant = false;
      continue;
    }
    if (item.type === "user" && !item.midTurn) {
      const started = item.timestamp ? Date.parse(item.timestamp) : NaN;
      turnStarted = Number.isFinite(started) ? started : null;
      sawAssistant = false;
      continue;
    }
    if (item.type !== "assistant" || sawAssistant || turnStarted == null || !item.timestamp) continue;
    const firstText = Date.parse(item.timestamp);
    if (Number.isFinite(firstText)) {
      out[item.id] = formatDuration(Math.max(0, firstText - turnStarted));
    }
    sawAssistant = true;
  }
  return out;
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
  mobile = false,
}: {
  timestamp?: string;
  side: "user" | "assistant";
  mobile?: boolean;
}) {
  if (!timestamp) return null;
  const full = formatFullDateTime(timestamp);
  return (
    <time
      className={`msg-time msg-time-${side}${mobile ? " msg-time-mobile" : ""}`}
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

function ReplyButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      className="msg-reply"
      title="Reply to message"
      aria-label="Reply to message"
      onClick={(event) => {
        event.stopPropagation();
        onClick();
      }}
    >
      <ReplyIcon size={15} />
      <span>Reply</span>
    </button>
  );
}

const FILE_REFERENCE = /`([^`\n]+)`|(?<![\w./-])([\w.-]+(?:\/[\w.-]+)*\.[A-Za-z0-9]+)\b/g;

function ReplyReference({
  author,
  quote,
  onOpenFile,
}: {
  author: string;
  quote: string;
  onOpenFile: (path: string) => void;
}) {
  const preview = replyPreview(quote, 280);
  const pieces: ReactNode[] = [];
  let cursor = 0;
  FILE_REFERENCE.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = FILE_REFERENCE.exec(preview))) {
    const path = match[1] ?? match[2];
    const start = match.index;
    if (start > cursor) pieces.push(preview.slice(cursor, start));
    pieces.push(
      <button
        key={`${path}-${start}`}
        type="button"
        className="msg-reply-reference-file"
        title={`Open ${path} in Files`}
        onClick={(event) => {
          event.stopPropagation();
          onOpenFile(path);
        }}
      >
        {path}
      </button>,
    );
    cursor = start + match[0].length;
  }
  if (cursor < preview.length) pieces.push(preview.slice(cursor));

  return (
    <div className="msg-reply-reference">
      <span className="msg-reply-reference-label">Replying to {author}</span>
      <span className="msg-reply-reference-quote">{pieces.length > 0 ? pieces : preview}</span>
    </div>
  );
}

function UserMessage({
  item,
  render,
}: {
  item: Extract<FeedItem, { type: "user" }>;
  render: FeedRenderContext;
}) {
  const threadId = render.threadId;
  const images = (item.attachments ?? []).filter((a) => isImageAttachment(a.name, a.mimeType));
  const reply = parseReply(item.text);
  const messageText = reply?.body ?? item.text;
  const hasText = messageText.trim().length > 0;
  // Claimed once, at first render: this bubble is the echo of a send from this
  // client, so it gets the send entrance (or the arcade theme's slam variant).
  // Scrollback and messages from other devices find the beacon empty and mount
  // silently.
  const [justSent] = useState(claimJustSent);
  return (
    <div className={`msg-user-wrap${justSent ? " just-sent" : ""}`}>
      {item.midTurn && <div className="msg-user-midturn">added while working</div>}
      {reply && (
        <ReplyReference
          author={reply.author}
          quote={reply.quote}
          onOpenFile={render.onOpenFile}
        />
      )}
      {hasText && <div className="msg-user"><div className="msg-user-text">{messageText}</div></div>}
      {render.http && threadId && images.length > 0 && (
        <div className={`msg-user-attachments ${images.length === 1 ? "single" : "gallery"}`}>
          {images.map((a) => {
            const url = attachmentUrl(render.http!, threadId, a.id, {
              machineId: render.machineId,
            });
            return <MessageImage key={a.id} url={url} name={a.name} />;
          })}
        </div>
      )}
      {(hasText || images.length > 0) && (
        <ReplyButton
          onClick={() =>
            render.onReply({
              id: item.id,
              kind: "user",
              author: "You",
              text: item.text,
              attachments: images,
              timestamp: item.timestamp,
            })
          }
        />
      )}
      <MessageTime timestamp={item.timestamp} side="user" />
    </div>
  );
}

// ---- dispatcher ---------------------------------------------------------

export const FeedItemView = memo(function FeedItemView({
  item,
  render,
  thoughtTime,
}: {
  item: FeedItem;
  render: FeedRenderContext;
  thoughtTime?: string;
}) {
  switch (item.type) {
    case "user":
      // A role brief is not the user talking — see BriefDivider.
      return item.injected
        ? <BriefDivider item={item} />
        : <UserMessage item={item} render={render} />;
    case "assistant":
      return (
        <div className={`msg-assistant${item.streaming ? " streaming" : ""}`}>
          <div className="assistant-head">
            <SpeakerChip speaker={item.speaker} participants={render.participants} />
            {thoughtTime && (
              <span className="assistant-thought-stat" title="Thought time">
                <BrainIcon size={12} />
                Thought for {thoughtTime}
              </span>
            )}
            {!item.streaming && (
              <MessageTime timestamp={item.timestamp} side="assistant" mobile />
            )}
          </div>
          <Markdown
            text={item.text}
            streaming={item.streaming}
            onOpenFile={render.onOpenFile}
          />
          {item.streaming && <span className="stream-caret" />}
          {!item.streaming && item.text.length > 0 && (
            <CopyButton value={item.text} label="Copy message" className="copy-btn msg-copy" />
          )}
          {!item.streaming && item.text.trim().length > 0 && (
            <ReplyButton
              onClick={() =>
                render.onReply({
                  id: item.id,
                  kind: "assistant",
                  author:
                    render.participants.find((participant) => participant.id === item.speaker)?.name ??
                    "Assistant",
                  text: item.text,
                  timestamp: item.timestamp,
                })
              }
            />
          )}
          {!item.streaming && (
            <MessageTime timestamp={item.timestamp} side="assistant" />
          )}
        </div>
      );
    case "thinking":
      return <ThinkingBlock item={item} />;
    case "tool":
      return <ToolRow item={item} render={render} />;
    case "diff":
      return <DiffCard item={item} render={render} />;
    case "artifact":
      return <ArtifactCard item={item} render={render} />;
    case "approval":
      return <ApprovalCard item={item} render={render} />;
    case "question":
      return (
        <QuestionCard
          item={item}
          onSetAnswers={render.actions.setQuestionAnswers}
          onRespond={render.actions.respondQuestion}
        />
      );
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
    case "failure":
      return (
        <div className="feed-failure" role="alert">
          <div className="feed-failure-title">⚠ {item.title}</div>
          <div className="feed-failure-message">{item.message}</div>
          {item.path && <code className="feed-failure-path">{item.path}</code>}
          {item.hint && <div className="feed-failure-hint">{item.hint}</div>}
        </div>
      );
    default:
      return null;
  }
});
