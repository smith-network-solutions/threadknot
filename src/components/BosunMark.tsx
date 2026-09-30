import { useStore } from "../state/store";
import { AnchorIcon } from "./icons";

/** The one Bosun mark: the bosun's avatar when it has an image, otherwise the
 *  anchor icon. Decorative — callers put the name in text or an aria-label,
 *  so this never renders the name itself (no alt text, no title). */
export function BosunMark({
  image,
  size = 16,
  className,
}: {
  image?: string | null;
  size?: number;
  className?: string;
}) {
  const cls = `bosun-mark${image ? " has-image" : ""}${className ? ` ${className}` : ""}`;
  return (
    <span className={cls} style={{ width: size, height: size }} aria-hidden="true">
      {image ? <img src={image} alt="" /> : <AnchorIcon size={size} />}
    </span>
  );
}

/** Image of the bosun with this id, if it has one and is loaded here. */
export function useBosunImage(bosunId?: string | null): string | null {
  const { state } = useStore();
  if (!bosunId) return null;
  return state.bosuns.find((b) => b.id === bosunId)?.image ?? null;
}
