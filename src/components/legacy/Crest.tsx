// The crest a finished run leaves behind, and the hook the chrome uses to ask
// whether one is due.
//
// It is a pixel map rather than an image file: nine rows of characters, drawn
// as SVG rects. That keeps it asset-free, scales to any size without a second
// export, and means the two tones can be re-coloured from CSS custom properties
// so it sits correctly on the skin card and in the sidebar foot.

import { useEffect, useState } from "react";
import {
  CREST_NAME,
  getLegacyAward,
  LEGACY_AWARD_EVENT,
  type LegacyAward,
} from "../../lib/legacyCircuit";

/** "#" is the shield, "*" is the interlace inside it. */
const CREST: readonly string[] = [
  "..#####..",
  ".#*****#.",
  "#**###**#",
  "#*#***#*#",
  "#**###**#",
  "#*#***#*#",
  ".#*****#.",
  "..#***#..",
  "...###...",
];

const SPAN = 9;

/** Mirror of the stored award, refreshed on every write, so a crest earned in
 *  the settings screen lights up the sidebar behind it without a reload. */
export function useLegacyAward(): LegacyAward {
  const [award, setAward] = useState<LegacyAward>(getLegacyAward);
  useEffect(() => {
    const onEvt = () => setAward(getLegacyAward());
    window.addEventListener(LEGACY_AWARD_EVENT, onEvt);
    // Another window of the same app (a solo project window) writes to the same
    // storage; `storage` is the only notice this one gets of that.
    window.addEventListener("storage", onEvt);
    return () => {
      window.removeEventListener(LEGACY_AWARD_EVENT, onEvt);
      window.removeEventListener("storage", onEvt);
    };
  }, []);
  return award;
}

export function Crest({
  size = 14,
  className = "",
  title,
}: {
  size?: number;
  className?: string;
  /** Omit for a purely decorative crest sitting next to its own label. */
  title?: string;
}) {
  return (
    <svg
      className={`legacy-crest ${className}`}
      width={size}
      height={size}
      viewBox={`0 0 ${SPAN} ${SPAN}`}
      role={title ? "img" : undefined}
      aria-label={title}
      aria-hidden={title ? undefined : true}
      shapeRendering="crispEdges"
    >
      {title && <title>{title}</title>}
      {CREST.map((row, y) =>
        row.split("").map((ch, x) =>
          ch === "." ? null : (
            <rect
              key={`${x}-${y}`}
              x={x}
              y={y}
              width={1}
              height={1}
              className={ch === "#" ? "crest-edge" : "crest-weave"}
            />
          ),
        ),
      )}
    </svg>
  );
}

/**
 * The crest as it appears in the app chrome: only for someone who has earned
 * it, and only while the Retro-Tech palette is on, because that is the skin it
 * belongs to. The tooltip names the award and nothing else: telling a visitor
 * how it was earned would give the circuit away.
 */
export function CrestBadge({ size = 14 }: { size?: number }) {
  const award = useLegacyAward();
  if (!award.earned) return null;
  return (
    <span className="legacy-crest-badge" title={`${CREST_NAME} · awarded on this machine`}>
      <Crest size={size} title={CREST_NAME} />
    </span>
  );
}
