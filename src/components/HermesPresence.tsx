import type { HermesAgentStatus } from "../lib/protocol";
import { timeAgo } from "../lib/format";

/** The three presence states a Hermes gateway can be shown in. A MISSING
 *  status (the health poller hasn't reported this agent yet) is "checking",
 *  never "offline": it renders neutral, not red. */
export type HermesPresenceKind = "online" | "offline" | "checking";

export interface HermesPresence {
  kind: HermesPresenceKind;
  /** Tooltip text: "online (123ms)" / "offline since 2m ago" / "checking…". */
  title: string;
  /** Inline label for text rows (settings list, gateway picker):
   *  "online · 123ms" / "offline · 2m ago" / "checking…". */
  label: string;
}

/** Derive the presence display for a gateway from its live status. Undefined
 *  (status not yet reported) maps to "checking", so a freshly-started agent
 *  reads as neutral rather than offline. */
export function hermesPresence(status: HermesAgentStatus | undefined): HermesPresence {
  if (!status) {
    return { kind: "checking", title: "checking…", label: "checking…" };
  }
  if (status.online) {
    const ms = status.latencyMs != null ? `${status.latencyMs}ms` : "";
    return {
      kind: "online",
      title: ms ? `online (${ms})` : "online",
      label: ms ? `online · ${ms}` : "online",
    };
  }
  // "offline since <when the outage began>", not the latest probe time —
  // sinceAt is preserved across polls so it reads the true outage start.
  const since = timeAgo(status.sinceAt || status.lastCheckedAt);
  return {
    kind: "offline",
    title: since ? `offline since ${since}` : "offline",
    label: since ? `offline · ${since}` : "offline",
  };
}

/** Overlay presence dot for an avatar corner. Belongs inside a
 *  `.hermes-avatar-wrap` (position:relative) alongside the avatar; pass
 *  `className="sm"` for the smaller thread-row avatars. */
export function HermesPresenceDot({
  status,
  className,
}: {
  status: HermesAgentStatus | undefined;
  className?: string;
}) {
  const p = hermesPresence(status);
  return (
    <span
      className={`hermes-presence ${p.kind}${className ? ` ${className}` : ""}`}
      title={p.title}
      aria-hidden
    />
  );
}
