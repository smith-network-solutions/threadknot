// Human-friendly cadence formatting + local next-occurrence math for
// scheduled runs. Mirrors the server's schedules.rs (0=Sunday..6=Saturday,
// local "HH:MM" times) so the form preview matches what will actually happen.

import type { Cadence } from "./protocol";

export const DAY_SHORT = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
export const DAY_CHIP = ["S", "M", "T", "W", "T", "F", "S"];

/** "08:30" → "8:30 AM". Falls back to the raw string if unparseable. */
export function formatTime(hhmm: string): string {
  const m = /^(\d{1,2}):(\d{2})$/.exec(hhmm);
  if (!m) return hhmm;
  const h = Number(m[1]);
  const ampm = h >= 12 ? "PM" : "AM";
  const h12 = h % 12 === 0 ? 12 : h % 12;
  return `${h12}:${m[2]} ${ampm}`;
}

/** "Weekdays at 8:30 AM", "Every 2 hours", "Mon, Wed, Fri at 9:00 AM"… */
export function cadenceLabel(c: Cadence): string {
  switch (c.type) {
    case "hourly":
      return c.everyHours <= 1 ? "Every hour" : `Every ${c.everyHours} hours`;
    case "daily":
      return `Daily at ${formatTime(c.time)}`;
    case "weekdays":
      return `Weekdays at ${formatTime(c.time)}`;
    case "weekly": {
      const days = [...c.days].sort((a, b) => a - b).map((d) => DAY_SHORT[d] ?? "?");
      if (days.length === 0) return `Weekly at ${formatTime(c.time)}`;
      return `${days.join(", ")} at ${formatTime(c.time)}`;
    }
  }
}

function nextAtTime(after: Date, hhmm: string, days: number[]): Date | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(hhmm);
  if (!m || days.length === 0) return null;
  for (let offset = 0; offset <= 14; offset++) {
    const t = new Date(after);
    t.setDate(t.getDate() + offset);
    t.setHours(Number(m[1]), Number(m[2]), 0, 0);
    if (days.includes(t.getDay()) && t.getTime() > after.getTime()) return t;
  }
  return null;
}

/** First firing strictly after `after` (defaults to now); local time. */
export function nextOccurrence(c: Cadence, after: Date = new Date()): Date | null {
  switch (c.type) {
    case "hourly": {
      const every = Math.min(24, Math.max(1, c.everyHours));
      const t = new Date(after);
      t.setMinutes(0, 0, 0);
      for (let i = 0; i < 48; i++) {
        t.setHours(t.getHours() + 1);
        if (t.getHours() % every === 0) return t;
      }
      return null;
    }
    case "daily":
      return nextAtTime(after, c.time, [0, 1, 2, 3, 4, 5, 6]);
    case "weekdays":
      return nextAtTime(after, c.time, [1, 2, 3, 4, 5]);
    case "weekly":
      return nextAtTime(after, c.time, c.days);
  }
}

/** "in 3m" / "in 14h" / "tomorrow at 8:30 AM" / "Mon at 9:00 AM". */
export function untilLabel(when: Date, now: Date = new Date()): string {
  const ms = when.getTime() - now.getTime();
  if (ms <= 0) return "now";
  const mins = Math.round(ms / 60_000);
  if (mins < 60) return `in ${Math.max(1, mins)}m`;
  if (mins < 12 * 60) return `in ${Math.round(mins / 60)}h`;
  const time = formatTime(
    `${when.getHours().toString().padStart(2, "0")}:${when.getMinutes().toString().padStart(2, "0")}`,
  );
  const tomorrow = new Date(now);
  tomorrow.setDate(tomorrow.getDate() + 1);
  if (when.toDateString() === now.toDateString()) return `today at ${time}`;
  if (when.toDateString() === tomorrow.toDateString()) return `tomorrow at ${time}`;
  return `${DAY_SHORT[when.getDay()]} at ${time}`;
}
