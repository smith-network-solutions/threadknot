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
  PERFECT_NAME,
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

/**
 * The finishing medals. One shape, three metals, because a medal you have to
 * read the label of is not a medal.
 *
 *   BRONZE  finished the campaign, 6 or 7 of the 9 levels taken
 *   SILVER  8 of 9: three clean games but one, where you dropped a single level
 *   GOLD    9 of 9, nothing lost
 *
 * Ribbon straps at the top, disc below, bright core inside the disc.
 */
const MEDAL: readonly string[] = [
  "##.......##",
  ".##.....##.",
  "..##...##..",
  "...#####...",
  "..#######..",
  ".###***###.",
  ".##*****##.",
  ".###***###.",
  "..#######..",
  "...#####...",
  "....###....",
];

/**
 * The AI ABUSER AWARD. Deliberately the ugliest thing in the file: a smug
 * little robot head in colours no self-respecting cabinet would use, because
 * the joke only lands if the badge itself looks like a punishment.
 */
const ABUSER: readonly string[] = [
  "....#.#....",
  "..#######..",
  ".#########.",
  "#.##...##.#",
  "#.#*#.#*#.#",
  "#.##...##.#",
  "#.#######.#",
  "#.#*****#.#",
  "#.#######.#",
  ".#########.",
  "..##...##..",
];

/** The face the cabinet pulls when you lose: crossed eyes and a flat frown,
 *  which is the entire emotional range a nine by nine grid affords. */
const DEAD: readonly string[] = [
  ".#######.",
  "#.......#",
  "#*.*.*.*#",
  "#.*...*.#",
  "#*.*.*.*#",
  "#.......#",
  "#.*...*.#",
  "#..***..#",
  ".#######.",
];

/** The Perfect Clear crest: a star burst rather than a shield, so the two are
 *  told apart at a glance and at a size where detail is gone. */
const PERFECT: readonly string[] = [
  "....#....",
  "...###...",
  "#..#*#..#",
  "###***###",
  ".#*****#.",
  "###***###",
  "#..#*#..#",
  "...###...",
  "....#....",
];

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
  variant = "weaver",
}: {
  size?: number;
  className?: string;
  /** Omit for a purely decorative crest sitting next to its own label. */
  title?: string;
  variant?: "weaver" | "perfect" | "dead" | "bronze" | "silver" | "gold" | "abuser";
}) {
  const map =
    variant === "perfect" ? PERFECT
    : variant === "dead" ? DEAD
    : variant === "abuser" ? ABUSER
    : variant === "bronze" || variant === "silver" || variant === "gold" ? MEDAL
    : CREST;
  return (
    <svg
      className={`legacy-crest legacy-crest-${variant} ${className}`}
      width={size}
      height={size}
      viewBox={`0 0 ${map[0].length} ${map.length}`}
      role={title ? "img" : undefined}
      aria-label={title}
      aria-hidden={title ? undefined : true}
      shapeRendering="crispEdges"
    >
      {title && <title>{title}</title>}
      {map.map((row, y) =>
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
    <span className="legacy-crest-badge">
      <span title={`${CREST_NAME} · awarded on this machine`}>
        <Crest size={size} title={CREST_NAME} />
      </span>
      {award.perfect && (
        <span
          title={perfectTooltip(award)}
          className={award.perfectHuman ? "perfect-verified" : "perfect-unverified"}
        >
          <Crest size={size} variant="perfect" title={PERFECT_NAME} />
        </span>
      )}
    </span>
  );
}

/** One line that says what the badge means without overclaiming what the
 *  verification actually establishes. */
export function perfectTooltip(award: LegacyAward): string {
  const who = award.perfectHandle ? ` by ${award.perfectHandle}` : "";
  return award.perfectHuman
    ? `${PERFECT_NAME}${who} · nine levels, no life lost · input looked human (a heuristic, not proof)`
    : `${PERFECT_NAME}${who} · nine levels, no life lost · input was not verified as human`;
}
