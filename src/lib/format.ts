/** "2h ago" style relative timestamps. */
export function timeAgo(iso: string): string {
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "";
  const s = Math.max(0, (Date.now() - t) / 1000);
  if (s < 45) return "now";
  const m = s / 60;
  if (m < 60) return `${Math.round(m)}m ago`;
  const h = m / 60;
  if (h < 24) return `${Math.round(h)}h ago`;
  const d = h / 24;
  if (d < 7) return `${Math.round(d)}d ago`;
  const w = d / 7;
  if (w < 5) return `${Math.round(w)}w ago`;
  const mo = d / 30;
  if (mo < 12) return `${Math.round(mo)}mo ago`;
  return `${Math.round(d / 365)}y ago`;
}

function validDate(iso: string): Date | null {
  const date = new Date(iso);
  return Number.isFinite(date.getTime()) ? date : null;
}

/** Compact local clock time for a chat message. */
export function formatMessageTime(iso: string): string {
  const date = validDate(iso);
  if (!date) return "";
  return new Intl.DateTimeFormat(undefined, {
    hour: "numeric",
    minute: "2-digit",
  }).format(date);
}

/** Local date/time that stays compact but disambiguates older days. */
export function formatCompactDateTime(iso: string): string {
  const date = validDate(iso);
  if (!date) return "";
  const now = new Date();
  const sameDay =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();
  return new Intl.DateTimeFormat(
    undefined,
    sameDay
      ? { hour: "numeric", minute: "2-digit" }
      : {
          month: "short",
          day: "numeric",
          year: date.getFullYear() === now.getFullYear() ? undefined : "numeric",
          hour: "numeric",
          minute: "2-digit",
        },
  ).format(date);
}

/** Full local date/time for hover text and assistive labels. */
export function formatFullDateTime(iso: string): string {
  const date = validDate(iso);
  if (!date) return "";
  return new Intl.DateTimeFormat(undefined, {
    weekday: "long",
    year: "numeric",
    month: "long",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
    second: "2-digit",
    timeZoneName: "short",
  }).format(date);
}

/** Human duration with task-scale precision. */
export function formatDuration(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 1) return "<1s";
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remSeconds = seconds % 60;
  if (minutes < 60) return remSeconds ? `${minutes}m ${remSeconds}s` : `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const remMinutes = minutes % 60;
  if (hours < 24) return remMinutes ? `${hours}h ${remMinutes}m` : `${hours}h`;
  const days = Math.floor(hours / 24);
  const remHours = hours % 24;
  return remHours ? `${days}d ${remHours}h` : `${days}d`;
}

export function formatTokens(n: number | undefined): string | null {
  if (n == null) return null;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 10_000) return `${Math.round(n / 1000)}k`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

/**
 * Shorten a path for display while preserving BOTH ends. The leading segments
 * identify the machine and the owner — a thread can be dispatched to another
 * harness or another box, so `/home/spencer/…` is what answers "whose device is
 * this running on" — and the final segment is the project. Only the middle,
 * which carries neither, is elided.
 *
 *   /home/spencer/WebstormProjects/threadknot  ->  /home/spencer/…/threadknot
 *
 * Segment-count based rather than width-measured: deterministic, and it costs
 * no layout read. Paths short enough to survive intact come back untouched.
 */
export function elidePathMiddle(path: string, keepHead = 2, keepTail = 1): string {
  const lead = path.startsWith("/") ? "/" : "";
  const parts = path.slice(lead.length).split("/").filter(Boolean);
  if (parts.length <= keepHead + keepTail) return path;
  const head = parts.slice(0, keepHead);
  const tail = parts.slice(parts.length - keepTail);
  return `${lead}${[...head, "…", ...tail].join("/")}`;
}

/** Copy text; falls back to execCommand for non-secure (http LAN) contexts. */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // fall through
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    return ok;
  } catch {
    return false;
  }
}
