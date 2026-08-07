import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { ReleaseNote } from "../lib/protocol";
import { useStore } from "../state/store";

/** "Jul 23, 2026" from a YYYY-MM-DD string. Parsed by hand so the date can't
 *  drift a day in negative-UTC timezones (new Date("2026-07-23") is UTC). */
function fmtDate(iso: string): string {
  const [y, m, d] = iso.split("-").map(Number);
  if (!y || !m || !d) return iso;
  return new Date(y, m - 1, d).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}

/** Sidebar-foot version label. Click to open the client-facing update notes
 *  (CHANGELOG.md, embedded at build time by build.rs) — no hover behavior.
 *  The version itself is git-derived, so every shipped change bumps it
 *  automatically. */
export function VersionBadge() {
  const { state, actions } = useStore();
  const [notes, setNotes] = useState<ReleaseNote[] | null>(null);
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{
    left: number;
    bottom: number;
    maxHeight: number;
  } | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  const popRef = useRef<HTMLDivElement>(null);

  // Fetch once, lazily, on first open.
  const load = () => {
    if (notes !== null) return;
    actions
      .fetchChangelog()
      .then((r) => setNotes(r.notes))
      .catch(() => setNotes([]));
  };

  // Anchor the portaled popover above the trigger (same approach as
  // UsageMeter: both render unzoomed, so viewport rects map 1:1).
  useLayoutEffect(() => {
    if (!open) return;
    function place() {
      const b = btnRef.current;
      if (!b) return;
      const r = b.getBoundingClientRect();
      setPos({
        left: Math.max(8, r.left - 4),
        bottom: Math.max(8, window.innerHeight - r.top + 8),
        maxHeight: Math.max(120, r.top - 24),
      });
    }
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      const t = e.target as Node;
      if (btnRef.current?.contains(t)) return;
      if (popRef.current?.contains(t)) return;
      setOpen(false);
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

  const hello = state.hello;
  // The date this update went live: newest release note, falling back to the
  // build's commit date for builds without a changelog entry yet.
  const liveDate = notes?.[0]?.date || hello?.buildDate || "";

  return (
    <>
      <button
        type="button"
        ref={btnRef}
        className="foot-version foot-version-btn"
        aria-label="Version and update notes"
        aria-expanded={open}
        onClick={() => {
          load();
          setOpen((v) => !v);
        }}
      >
        {hello ? `v${hello.version}` : "…"}
      </button>

      {/* Click: client-facing update notes, newest first. */}
      {open &&
        pos &&
        createPortal(
          <div
            className="changelog-pop"
            role="dialog"
            aria-label="Update notes"
            ref={popRef}
            style={{ left: pos.left, bottom: pos.bottom, maxHeight: pos.maxHeight }}
          >
            <div className="changelog-head">
              <span>update notes</span>
              {liveDate && <span className="changelog-build">latest {fmtDate(liveDate)}</span>}
            </div>
            {notes === null && <div className="changelog-empty">loading…</div>}
            {notes !== null && notes.length === 0 && (
              <div className="changelog-empty">no update notes in this build</div>
            )}
            {(notes ?? []).map((r, i) => (
              <div key={`${r.version}-${r.date}-${i}`} className="changelog-entry">
                <div className="changelog-entry-head">
                  <span className="changelog-ver">{r.version ? `v${r.version}` : "update"}</span>
                  {r.date && <span className="changelog-meta">{fmtDate(r.date)}</span>}
                </div>
                <ul className="changelog-notes">
                  {r.notes.map((n, j) => (
                    <li key={j}>{n}</li>
                  ))}
                </ul>
              </div>
            ))}
          </div>,
          document.body,
        )}
    </>
  );
}
