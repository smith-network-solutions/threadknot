import { useRef, type ReactNode } from "react";

/** Horizontal review gestures leave vertical document scrolling and zoomed
 * image panning alone. Buttons also work for embedded documents and videos. */
export function ArtifactNavigation({ index, count, onSelect, children }: {
  index: number; count: number; onSelect: (index: number) => void; children: ReactNode;
}) {
  const start = useRef<{ id: number; x: number; y: number } | null>(null);
  return <div className="artifact-gallery">
    <div className="artifact-gallery-content"
      onPointerDownCapture={event => {
        if (start.current || !event.isPrimary) { start.current = null; return; }
        if (event.pointerType !== "touch" || (event.target as Element).closest("button, input, textarea, video, .is-zoomed")) return;
        start.current = { id: event.pointerId, x: event.clientX, y: event.clientY };
      }}
      onPointerCancelCapture={() => { start.current = null; }}
      onPointerUpCapture={event => {
        const from = start.current; start.current = null;
        if (!from || from.id !== event.pointerId || (event.target as Element).closest(".is-zoomed")) return;
        const dx = event.clientX - from.x, dy = event.clientY - from.y;
        if (Math.abs(dx) < 65 || Math.abs(dx) < Math.abs(dy) * 1.5) return;
        const next = index + (dx < 0 ? 1 : -1);
        if (next >= 0 && next < count) onSelect(next);
      }}
    >{children}</div>
    {count > 1 && <nav className="artifact-gallery-nav" aria-label="Artifact navigation">
      <button type="button" disabled={index <= 0} onClick={() => onSelect(index - 1)} aria-label="Previous artifact">← Previous</button>
      <span aria-live="polite">{index + 1} / {count}</span>
      <button type="button" disabled={index >= count - 1} onClick={() => onSelect(index + 1)} aria-label="Next artifact">Next →</button>
    </nav>}
  </div>;
}
