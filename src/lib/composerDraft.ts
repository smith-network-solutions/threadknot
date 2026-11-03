// The shapes the composer's two renders share: the default bar in
// components/Composer.tsx and the Alcove skin's card in
// components/AlcoveComposer.tsx. They live here rather than in either one so
// the skin's composer doesn't have to import from the component that renders
// it — a cycle that resolves at type-check time but not reliably at runtime.
import type { ReactNode } from "react";

/** One file staged on a draft, already base64-encoded for the wire. */
export interface DraftAttachment {
  localId: string;
  name: string;
  mimeType: string;
  data: string; // base64
}

export function attachmentDataUrl(attachment: { mimeType: string; data: string }): string {
  return `data:${attachment.mimeType};base64,${attachment.data}`;
}

/** One selectable value inside a settings row's flyout. */
export interface SettingOption {
  value: string;
  label: string;
  glyph?: ReactNode; // brand mark for agent options
  dot?: string; // hermes presence kind — a coloured status pip
  tag?: string; // trailing note ("offline")
  selected: boolean;
  onSelect: () => void;
}

/**
 * One extra section of the AI menu — a per-session setting that belongs to the
 * AI rather than to the message being written (context window, Chrome).
 */
export interface SettingRow {
  key: string;
  icon: ReactNode;
  label: string;
  valueLabel?: string;
  valueGlyph?: ReactNode;
  hint?: string;
  disabled?: boolean; // shown, but not changeable (locked value, or mid-turn)
  options: SettingOption[];
  action?: () => void; // a plain command row (e.g. "Claude only") — no choices
}

export interface PickerOption<T extends string> {
  id: T;
  icon: ReactNode;
  title: string;
  /** Trigger label on phone widths, where the bar has no room for the full one. */
  shortTitle?: string;
  desc: string;
}
