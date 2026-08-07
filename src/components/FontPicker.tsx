import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { MONO_FONTS, UI_FONTS, type FontOption } from "../lib/appearance";
import { ensureFontsLoaded } from "../lib/fonts";
import { ChevronIcon } from "./icons";

/**
 * A custom font dropdown that renders every option IN its own typeface — the
 * one thing a native <select> can't do. Mechanics mirror AgentSelect in
 * Composer.tsx (button trigger + absolute menu, outside-click / Escape close),
 * plus roving keyboard focus over the long list.
 *
 * API (kept deliberately small so the ThemeStudio redesign can drop it in for
 * both the interface- and code-font rows):
 *   kind      — "ui" reads UI_FONTS, "mono" reads MONO_FONTS.
 *   value     — the selected font id.
 *   onChange  — called with the chosen font id.
 *   direction — "down" (default) opens below the trigger, "up" opens above.
 *
 * Keyboard: when closed, Enter / Space / ArrowUp / ArrowDown open the list.
 * When open, ArrowUp/ArrowDown move the highlight (and scroll it into view),
 * Home/End jump to the ends, Enter / Space pick the highlighted font, Escape
 * closes without changing the value.
 */

/** Previewed in each option so a font's character shapes read at a glance:
 *  letters + digits for UI, glyphs a coder recognises for mono. */
const SAMPLE: Record<"ui" | "mono", string> = {
  ui: "Ag 123",
  mono: "{ } =>",
};

export function FontPicker({
  kind,
  value,
  onChange,
  direction = "down",
}: {
  kind: "ui" | "mono";
  value: string;
  onChange: (id: string) => void;
  direction?: "up" | "down";
}) {
  const options: readonly FontOption[] = kind === "ui" ? UI_FONTS : MONO_FONTS;
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const ref = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const current = options.find((f) => f.id === value) ?? options[0];
  const sample = SAMPLE[kind];

  // On open: request every family so the previews fill in as the (small,
  // cached) CSS files arrive, and seed the highlight on the current value.
  useEffect(() => {
    if (!open) return;
    ensureFontsLoaded(options);
    const idx = options.findIndex((f) => f.id === value);
    setActive(idx < 0 ? 0 : idx);
  }, [open, options, value]);

  // Outside-click closes (mousedown, matching AgentSelect).
  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  // Escape closes only the picker, not the Settings screen underneath. Caught
  // in the capture phase with stopPropagation — the same idiom MachineCardMenu
  // uses in this same screen — so it lands before the settings screen's own
  // document-level Escape listener ever sees it.
  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      setOpen(false);
    }
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open]);

  // Keep the highlighted row visible as the keyboard walks the long list.
  useEffect(() => {
    if (!open) return;
    listRef.current
      ?.querySelector<HTMLElement>(`[data-idx="${active}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [open, active]);

  function choose(id: string) {
    onChange(id);
    setOpen(false);
  }

  function onKeyDown(e: ReactKeyboardEvent) {
    if (!open) {
      if (e.key === "Enter" || e.key === " " || e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        setOpen(true);
      }
      return;
    }
    switch (e.key) {
      // Escape is handled by the capture-phase window listener above (so it can
      // stopPropagation before the settings screen's document listener runs).
      case "ArrowDown":
        e.preventDefault();
        setActive((i) => Math.min(options.length - 1, i + 1));
        break;
      case "ArrowUp":
        e.preventDefault();
        setActive((i) => Math.max(0, i - 1));
        break;
      case "Home":
        e.preventDefault();
        setActive(0);
        break;
      case "End":
        e.preventDefault();
        setActive(options.length - 1);
        break;
      case "Enter":
      case " ":
        e.preventDefault();
        choose(options[active].id);
        break;
    }
  }

  return (
    <div className="font-picker" ref={ref} onKeyDown={onKeyDown}>
      <button
        type="button"
        className="font-trigger"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={`${kind === "ui" ? "Interface" : "Code"} font — ${current.label}`}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="font-trigger-label" style={{ fontFamily: current.stack }}>
          {current.label}
        </span>
        <ChevronIcon size={11} open={open} className="row-chevron" />
      </button>
      {open && (
        <div
          ref={listRef}
          className={`font-menu${direction === "up" ? " up" : ""}`}
          role="listbox"
          aria-label={kind === "ui" ? "Interface font" : "Code font"}
        >
          {options.map((f, i) => (
            <button
              type="button"
              key={f.id}
              data-idx={i}
              role="option"
              aria-selected={f.id === value}
              className={`font-option${f.id === value ? " on" : ""}${i === active ? " active" : ""}`}
              style={{ fontFamily: f.stack }}
              onMouseEnter={() => setActive(i)}
              onClick={() => choose(f.id)}
            >
              <span className="font-option-label">{f.label}</span>
              <span className="font-option-sample">{sample}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
