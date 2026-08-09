import type {
  Agent,
  AgentEvent,
  ApprovalKind,
  AttachmentMeta,
  DispatchAttribution,
  ApprovalOption,
  Question,
  TurnUsage,
} from "../lib/protocol";

import { AGENT_LABELS as AGENT_NAMES } from "../lib/protocol";

let nextItemId = 1;
function iid(): string {
  return `i${nextItemId++}`;
}

/** Live state of a subagent launched by a Task / background Agent tool call. */
export interface SubagentInfo {
  taskId: string;
  description: string;
  subagentType: string;
  background: boolean;
  status: "running" | "completed" | "error";
  prompt?: string;
  startedAt?: string;
  lastActivityAt?: string;
  completedAt?: string;
  summary?: string;
  /** Rolling tail of recent activity lines (synchronous subagents stream these). */
  activity: { activity: string; text: string }[];
  /** Set when this is a dispatched worker rather than a provider's own
   *  subagent: which machine and harness it runs on, and the thread to open. */
  dispatch?: DispatchAttribution;
}

const SUBAGENT_ACTIVITY_TAIL = 8;


function isAgentTool(name: string): boolean {
  return name === "Agent" || /^Launching .* agent(?::|$)/i.test(name);
}

function agentToolInput(detail: string): { description?: string; subagentType?: string; prompt?: string } {
  if (!detail.trim().startsWith("{")) return {};
  try {
    const value = JSON.parse(detail) as Record<string, unknown>;
    return {
      description: typeof value.description === "string" ? value.description : undefined,
      subagentType: typeof value.subagent_type === "string" ? value.subagent_type : undefined,
      prompt: typeof value.prompt === "string" ? value.prompt : undefined,
    };
  } catch {
    return {};
  }
}

function fallbackAgentResult(output: string, isError: boolean): Pick<SubagentInfo, "status" | "summary"> {
  const status = output.match(/^status:\s*(\S+)/m)?.[1]?.toLowerCase();
  const summary = output.split("[summary]")[1]?.trim() || undefined;
  return {
    status: isError || (status != null && status !== "completed") ? "error" : "completed",
    summary,
  };
}

/**
 * Old Kimi backends persisted one whole-turn message after every tool, even
 * though its pieces had already streamed around those tools. Once tool starts
 * finalize streams (below), remove those exact prior pieces from that legacy
 * aggregate so the final message contains only its genuinely new tail.
 */
function stripLegacyAggregate(
  items: FeedItem[],
  type: "assistant" | "thinking",
  text: string,
): string {
  let boundary = 0;
  for (let i = items.length - 1; i >= 0; i--) {
    if ((items[i].type === "user" && !(items[i] as Extract<FeedItem, { type: "user" }>).midTurn)
        || items[i].type === "turn_end") {
      boundary = i + 1;
      break;
    }
  }
  const prefix = items
    .slice(boundary)
    .filter((item): item is Extract<FeedItem, { type: typeof type }> =>
      item.type === type && !item.streaming,
    )
    .map((item) => item.text)
    .join("");
  return prefix && text.startsWith(prefix) ? text.slice(prefix.length) : text;
}

/**
 * Subagents belonging to the current turn (everything since the last user
 * message), newest-relevant first is left to the caller. Powers the pinned
 * agent HUD so long-running background agents stay reachable after their cards
 * scroll out of view.
 */
export function activeSubagents(items: FeedItem[]): SubagentInfo[] {
  let start = 0;
  for (let i = items.length - 1; i >= 0; i--) {
    const it = items[i];
    // A mid-turn steering note doesn't start a turn; keep scanning back so
    // agents launched before the note stay in the HUD.
    if (it.type === "user" && !it.midTurn) {
      start = i;
      break;
    }
  }
  const out: SubagentInfo[] = [];
  const seen = new Set<string>();
  for (let i = start; i < items.length; i++) {
    const it = items[i];
    if (it.type === "tool" && it.subagent) {
      out.push(it.subagent);
      seen.add(it.subagent.taskId);
    }
  }
  // A dispatched worker outlives the turn that sent it — `dispatch` returns
  // immediately and the planner's turn ends while the worker is still going, so
  // scoping strictly to the current turn would make live workers vanish from
  // the HUD the moment the user says anything else. Anything still running is
  // still worth showing, whichever turn started it.
  for (let i = 0; i < start; i++) {
    const it = items[i];
    if (
      it.type === "tool" &&
      it.subagent?.status === "running" &&
      !seen.has(it.subagent.taskId)
    ) {
      out.push(it.subagent);
      seen.add(it.subagent.taskId);
    }
  }
  return out;
}

interface FeedItemBase {
  id: string;
  /** ISO event time from the thread owner; exact on replay and live frames. */
  timestamp?: string;
  /** Lane that produced this item (`Participant.id`). Absent on the user's own
   *  messages, on server-issued notes, and throughout any thread recorded
   *  before Parley — so the UI must treat it as optional, not as "the builder". */
  speaker?: string;
}

export type FeedItem = FeedItemBase & (
  | {
      type: "user";
      text: string;
      attachments?: AttachmentMeta[];
      /** Note submitted while work is active — not a Threadknot turn boundary. */
      midTurn?: boolean;
      /** A machine-issued brief (a Parley role prompt), not the human talking.
       *  Deliberately still a `user` item: it IS a real turn boundary, and every
       *  boundary scan in this module keys off `type === "user"`. Only the
       *  rendering differs. */
      injected?: boolean;
    }
  | { type: "assistant"; text: string; streaming: boolean }
  | { type: "thinking"; text: string; streaming: boolean }
  | {
      type: "tool";
      callId: string;
      name: string;
      detail: string;
      output: string;
      isError: boolean;
      done: boolean;
      /** Output arrived elided from a replay; expanding the card fetches the rest. */
      truncated?: boolean;
      /** Set when this tool call launched a subagent (Task / background Agent);
       *  drives the nested live-activity panel. */
      subagent?: SubagentInfo;
    }
  | { type: "diff"; path: string; unified: string }
  | {
      type: "artifact";
      artifactId: string;
      name: string;
      relPath: string;
      mimeType: string;
      sizeBytes: number;
      op: string;
      description?: string;
    }
  | {
      type: "approval";
      approvalId: string;
      approvalKind: ApprovalKind;
      title: string;
      detail: string;
      options: ApprovalOption[];
      resolvedOptionId?: string;
      pending?: boolean;
    }
  | {
      type: "question";
      requestId: string;
      questions: Question[];
      answers: Record<string, string[]>; // locally-chosen, keyed by question.id
      answered: boolean;
      pending?: boolean;
    }
  | { type: "turn_end"; usage?: TurnUsage; aborted?: boolean; durationMs?: number }
  /** Latest context snapshot; retained in replay but intentionally not rendered. */
  | { type: "context_usage"; usage: TurnUsage }
  /** Provenance marker for a turn; renders as a handoff divider when `switched`. */
  | { type: "turn"; agent?: Agent; model?: string; switched: boolean }
  | { type: "note"; noteKind: "status" | "error" | "session"; text: string }
);

function finalizeStreams(items: FeedItem[]): FeedItem[] {
  let changed = false;
  const out = items.map((it) => {
    if ((it.type === "assistant" || it.type === "thinking") && it.streaming) {
      changed = true;
      return { ...it, streaming: false };
    }
    return it;
  });
  return changed ? out : items;
}

function replaceLastStreaming(
  items: FeedItem[],
  type: "assistant" | "thinking",
  text: string,
  streaming: boolean,
  append: boolean,
  timestamp?: string,
): FeedItem[] {
  for (let i = items.length - 1; i >= 0; i--) {
    const it = items[i];
    if (it.type === type && it.streaming) {
      const next = items.slice();
      next[i] = {
        ...it,
        text: append ? it.text + text : text,
        streaming,
        timestamp: timestamp ?? it.timestamp,
      };
      return next;
    }
    // Stop scanning past a hard boundary — a new turn started since.
    if (it.type === "user" || it.type === "turn_end") break;
  }
  return [...items, { id: iid(), type, text, streaming, timestamp }];
}

function turnDuration(items: FeedItem[], timestamp?: string): number | undefined {
  if (!timestamp) return undefined;
  const ended = Date.parse(timestamp);
  if (!Number.isFinite(ended)) return undefined;
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    // Mid-turn notes are not turn boundaries: keep measuring from the message
    // that opened the turn.
    if (item.type === "user" && !item.midTurn && item.timestamp) {
      const started = Date.parse(item.timestamp);
      return Number.isFinite(started) ? Math.max(0, ended - started) : undefined;
    }
    if (item.type === "turn_end") break;
  }
  return undefined;
}

/**
 * Fold one agent event into the feed item list (pure; returns a new array).
 * `speaker` attributes the event to a lane — see [`stampSpeaker`].
 */
export function applyEvent(
  items: FeedItem[],
  ev: AgentEvent,
  timestamp?: string,
  speaker?: string,
): FeedItem[] {
  return stampSpeaker(items, foldEvent(items, ev, timestamp), speaker);
}

/**
 * Attribute the items this event created or replaced to its lane.
 *
 * The fold only ever touches the tail of the list, so walking back from the end
 * until the objects are reference-identical to the previous array finds exactly
 * what changed — no need to thread `speaker` through all ~30 fold branches.
 * Already-stamped items are never overwritten, so `finalizeStreams` rewriting
 * an earlier lane's message can't reassign it.
 */
function stampSpeaker(prev: FeedItem[], next: FeedItem[], speaker?: string): FeedItem[] {
  if (!speaker || next === prev) return next;
  const out = next.slice();
  for (let i = out.length - 1; i >= 0; i--) {
    if (prev[i] && prev[i] === out[i]) break;
    if (out[i].speaker === undefined) out[i] = { ...out[i], speaker };
  }
  return out;
}

function foldEvent(items: FeedItem[], ev: AgentEvent, timestamp?: string): FeedItem[] {
  switch (ev.kind) {
    case "user_message":
      return [
        ...finalizeStreams(items),
        {
          id: iid(),
          type: "user",
          text: ev.text,
          attachments: ev.attachments,
          midTurn: ev.mid_turn,
          injected: ev.injected,
          timestamp,
        },
      ];
    case "turn_started": {
      if (!ev.agent) return items; // legacy events carry no provenance
      // A divider only when provenance actually changed vs the previous turn.
      let switched = false;
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (it.type === "turn") {
          switched = it.agent !== ev.agent || (it.model ?? "") !== (ev.model ?? "");
          break;
        }
      }
      return [
        ...items,
        { id: iid(), type: "turn", agent: ev.agent, model: ev.model, switched, timestamp },
      ];
    }
    case "assistant_delta":
      return replaceLastStreaming(items, "assistant", ev.text, true, true, timestamp);
    case "assistant_message": {
      const text = stripLegacyAggregate(items, "assistant", ev.text);
      return text ? replaceLastStreaming(items, "assistant", text, false, false, timestamp) : items;
    }
    case "thinking_delta":
      return replaceLastStreaming(items, "thinking", ev.text, true, true, timestamp);
    case "thinking": {
      const text = stripLegacyAggregate(items, "thinking", ev.text);
      return text ? replaceLastStreaming(items, "thinking", text, false, false, timestamp) : items;
    }
    case "tool_start": {
      const input = agentToolInput(ev.detail);
      const subagent: SubagentInfo | undefined = isAgentTool(ev.name)
        ? {
            taskId: `tool:${ev.callId}`,
            description: input.description ?? "Kimi child agent",
            subagentType: input.subagentType ?? "agent",
            background: false,
            status: "running",
            prompt: input.prompt,
            startedAt: timestamp,
            activity: [],
          }
        : undefined;
      return [
        ...finalizeStreams(items),
        {
          id: iid(),
          type: "tool",
          callId: ev.callId,
          name: ev.name,
          detail: ev.detail,
          output: "",
          isError: false,
          done: false,
          timestamp,
          subagent,
        },
      ];
    }
    case "tool_output_delta": {
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (it.type === "tool" && it.callId === ev.callId) {
          const next = items.slice();
          next[i] = { ...it, output: it.output + ev.text };
          return next;
        }
      }
      return items;
    }
    case "tool_end": {
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (it.type === "tool" && it.callId === ev.callId) {
          const next = items.slice();
          next[i] = {
            ...it,
            name: ev.name || it.name,
            output: ev.output != null ? ev.output : it.output,
            isError: ev.isError ?? false,
            done: true,
            truncated: ev.truncated,
            subagent:
              // Legacy Kimi backends have no structured child lifecycle, so
              // their synthetic `tool:<callId>` child finishes with the Agent
              // tool itself. Claude (and current Kimi) emit a real task id
              // before this tool result; that result only says the background
              // child LAUNCHED. Finishing a structured child here makes the
              // HUD disappear while the agent is still working.
              it.subagent?.status === "running" &&
              it.subagent.taskId === `tool:${ev.callId}`
                ? {
                    ...it.subagent,
                    ...fallbackAgentResult(ev.output ?? it.output, ev.isError ?? false),
                    completedAt: timestamp,
                  }
                : it.subagent,
          };
          return next;
        }
      }
      // tool_end without a matching start (e.g. replay trimming) — synthesize.
      return [
        ...items,
        {
          id: iid(),
          type: "tool",
          callId: ev.callId,
          name: ev.name,
          detail: "",
          output: ev.output ?? "",
          isError: ev.isError ?? false,
          done: true,
          truncated: ev.truncated,
        },
      ];
    }
    case "file_diff":
      return [...items, { id: iid(), type: "diff", path: ev.path, unified: ev.unified }];
    case "artifact":
      return [
        ...items,
        {
          id: iid(),
          type: "artifact",
          artifactId: ev.id,
          name: ev.name,
          relPath: ev.relPath,
          mimeType: ev.mimeType,
          sizeBytes: ev.sizeBytes,
          op: ev.op,
          description: ev.description,
        },
      ];
    case "approval_request":
      return [
        ...items,
        {
          id: iid(),
          type: "approval",
          approvalId: ev.approvalId,
          approvalKind: ev.approvalKind,
          title: ev.title,
          detail: ev.detail,
          options: ev.options,
        },
      ];
    case "approval_resolved": {
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (it.type === "approval" && it.approvalId === ev.approvalId) {
          const next = items.slice();
          next[i] = { ...it, resolvedOptionId: ev.optionId, pending: false };
          return next;
        }
      }
      return items;
    }
    case "question_request": {
      // Dedupe: a request may already exist (replay / double-fire).
      if (items.some((it) => it.type === "question" && it.requestId === ev.requestId)) {
        return items;
      }
      return [
        ...items,
        {
          id: iid(),
          type: "question",
          requestId: ev.requestId,
          questions: ev.questions,
          answers: {},
          answered: false,
        },
      ];
    }
    case "question_resolved": {
      let found = false;
      const next = items.map((it) => {
        if (it.type === "question" && it.requestId === ev.requestId) {
          found = true;
          return { ...it, answered: true, pending: false };
        }
        return it;
      });
      return found ? next : items;
    }
    case "context_usage":
      return [...items, { id: iid(), type: "context_usage", usage: ev.usage }];
    case "turn_completed": {
      const durationMs = turnDuration(items, timestamp);
      return [
        ...finalizeStreams(items),
        { id: iid(), type: "turn_end", usage: ev.usage, timestamp, durationMs },
      ];
    }
    case "turn_aborted": {
      const durationMs = turnDuration(items, timestamp);
      return [
        ...finalizeStreams(items),
        { id: iid(), type: "turn_end", aborted: true, timestamp, durationMs },
      ];
    }
    case "session_started": {
      const who = ev.agent ? `${AGENT_NAMES[ev.agent]} · ` : "";
      return [
        ...items,
        { id: iid(), type: "note", noteKind: "session", text: `session started · ${who}${ev.model}` },
      ];
    }
    case "subagent_started": {
      const info: SubagentInfo = {
        taskId: ev.taskId,
        description: ev.description,
        subagentType: ev.subagentType,
        background: ev.background,
        status: "running",
        prompt: ev.prompt,
        startedAt: timestamp,
        activity: [],
        dispatch: ev.dispatch,
      };
      // Attach to the launching tool card (its callId is the toolUseId).
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (it.type === "tool" && it.callId === ev.toolUseId) {
          const next = items.slice();
          next[i] = {
            ...it,
            subagent: {
              ...it.subagent,
              ...info,
              startedAt: it.subagent?.startedAt ?? timestamp,
            },
          };
          return next;
        }
      }
      // No launching card seen (e.g. replay trimming) — synthesize one.
      return [
        ...items,
        {
          id: iid(),
          type: "tool",
          callId: ev.toolUseId || ev.taskId,
          // A dispatch has no launching tool card by design — there is no
          // provider tool call behind it — so this synthesized card is its
          // normal home, not a replay-trimming fallback.
          name: ev.dispatch ? "Dispatched" : ev.background ? "Agent" : "Task",
          detail: ev.description,
          output: "",
          isError: false,
          done: false,
          subagent: info,
        },
      ];
    }
    case "subagent_progress": {
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (it.type === "tool" && it.subagent?.taskId === ev.taskId) {
          const activity = [
            ...it.subagent.activity,
            { activity: ev.activity, text: ev.text },
          ].slice(-SUBAGENT_ACTIVITY_TAIL);
          const next = items.slice();
          next[i] = {
            ...it,
            subagent: { ...it.subagent, activity, lastActivityAt: timestamp },
          };
          return next;
        }
      }
      return items;
    }
    case "subagent_completed": {
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (it.type === "tool" && it.subagent?.taskId === ev.taskId) {
          const next = items.slice();
          next[i] = {
            ...it,
            subagent: {
              ...it.subagent,
              status: ev.status === "completed" ? "completed" : "error",
              summary: ev.summary ?? it.subagent.summary,
              completedAt: timestamp,
            },
          };
          return next;
        }
      }
      return items;
    }
    case "status":
      return [...items, { id: iid(), type: "note", noteKind: "status", text: ev.text }];
    case "error":
      return [
        ...finalizeStreams(items),
        { id: iid(), type: "note", noteKind: "error", text: ev.message },
      ];
    default:
      return items;
  }
}
