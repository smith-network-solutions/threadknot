// The Alcove skin's composer: the structure of the design prototype's
// Composer (C:/Projects/alcove/src/ui/components/Composer.tsx) over
// Threadknot's real wiring. Composer.tsx computes every value and handler and
// hands them here, so this file is presentation only — it owns no draft state,
// no send path and no settings of its own.
//
// Where the prototype hardcoded a mock, this reads the real thing:
//   its fake `models` list          -> the live agent/model rosters
//   its local `permission` useState -> the thread's Access setting
//   its fixed 4-notch effort slider -> the current model's own effort options
//   its browser SpeechRecognition   -> the server's dictation (voice/stt.rs),
//                                      falling back to the prototype's own
//                                      in-webview capture (browserDictation.ts)
//                                      on a server without the sidecar
// The one genuinely new thing is the mode row: the prototype has no Plan/Build
// split, and Threadknot's is too central to leave out of the only menu the
// skin gives the user.
import type { CSSProperties, ReactNode } from "react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import {
  Add01Icon,
  ArrowDown01Icon,
  ArrowUp02Icon,
  Cancel01Icon,
  FileUploadIcon,
  GlobalSearchIcon,
  ImageUploadIcon,
  Mic01Icon,
} from "@hugeicons/core-free-icons";
import type { Access, Mode } from "../lib/protocol";
import { isImageAttachment } from "../lib/attachments";
import { liveDictationStream } from "../lib/browserDictation";
import { CornerSmoothing } from "./alcove/CornerSmoothing";
import { Dropdown } from "./alcove/Dropdown";
import { Tooltip } from "./alcove/Tooltip";
import { ClockIcon, VoiceWaveIcon } from "./icons";
import { attachmentDataUrl } from "../lib/composerDraft";
import type {
  DraftAttachment,
  PickerOption,
  SettingOption,
  SettingRow,
} from "../lib/composerDraft";

/** Threadknot's dictation states, as Composer.tsx tracks them. */
export type MicState = "idle" | "starting" | "recording" | "transcribing";

export interface AlcoveComposerProps {
  /* ---- the draft ---- */
  text: string;
  placeholder: string;
  /** Composer.tsx's ref callback — it owns the auto-resize observer. */
  textareaRef: (node: HTMLTextAreaElement | null) => void;
  /** The syntax-highlighted copy painted behind the transparent textarea. */
  highlighted: ReactNode;
  backdropRef: (node: HTMLDivElement | null) => void;
  onTextChange: (value: string) => void;
  onCursorChange: (textarea: HTMLTextAreaElement) => void;
  onPaste: (event: React.ClipboardEvent<HTMLTextAreaElement>) => void;
  onKeyDown: (event: React.KeyboardEvent<HTMLTextAreaElement>) => void;

  /* ---- send / stop / queue ---- */
  onSubmit: () => void;
  onInterrupt: () => void;
  canSend: boolean;
  running: boolean;
  /** A live turn with a follow-up typed: the primary button queues. */
  queues: boolean;
  /** A live turn the note can reach mid-flight: the primary button sends. */
  steersNow: boolean;

  /* ---- attachments ---- */
  attachments: DraftAttachment[];
  attachFull: boolean;
  onAddFiles: (files: File[]) => void;
  onRemoveAttachment: (localId: string) => void;

  /* ---- the AI menu ---- */
  agent: string;
  agentMark: ReactNode;
  agentOptions: SettingOption[];
  showAgents: boolean;
  modelGroupLabel: string;
  modelOptions: SettingOption[];
  effortOptions: SettingOption[];
  /** Context window, Chrome — settings that belong to the AI. */
  sections: SettingRow[];
  /** Absent for remote Hermes agents, which govern approvals server-side. */
  access: Access | null;
  accessOptions: readonly PickerOption<Access>[];
  onAccessChange: (access: Access) => void;
  mode: Mode | null;
  modeOptions: readonly PickerOption<Mode>[];
  onModeChange: (mode: Mode) => void;

  /* ---- dictation + voice ---- */
  dictationAvailable: boolean;
  mic: MicState;
  micSeconds: number;
  onStartDictation: () => void;
  onStopDictation: () => void;
  voice?: { available: boolean };
  onVoice?: () => void;
  voiceLive?: boolean;

  /* ---- rows above the field (reply context, slash menu, queued, errors) ---- */
  children?: ReactNode;
  /** Rendered outside the glass card, above it. */
  banners?: ReactNode;
  fileDragActive: boolean;
  dragHandlers: {
    onDragEnter: (event: React.DragEvent<HTMLElement>) => void;
    onDragOver: (event: React.DragEvent<HTMLElement>) => void;
    onDragLeave: (event: React.DragEvent<HTMLElement>) => void;
    onDrop: (event: React.DragEvent<HTMLElement>) => void;
  };
}

/**
 * The prototype's cross-fade: a value changes, the old one blurs out, the new
 * one blurs in. Returns the value to paint plus whether it is mid-swap.
 */
function useBlurSwap<T>(value: T) {
  const [visibleValue, setVisibleValue] = useState(value);
  const [isSwapping, setIsSwapping] = useState(false);

  useEffect(() => {
    if (visibleValue === value) {
      setIsSwapping(false);
      return;
    }
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      setVisibleValue(value);
      return;
    }
    setIsSwapping(true);
    const timer = window.setTimeout(() => {
      setVisibleValue(value);
      setIsSwapping(false);
    }, 150);
    return () => window.clearTimeout(timer);
  }, [value, visibleValue]);

  return [visibleValue, isSwapping] as const;
}

/**
 * The prototype's live microphone waveform. Purely cosmetic and deliberately
 * fail-silent: if no capture can be drawn the band stays flat and dictation is
 * unaffected, because the transcript never runs through here.
 *
 * It prefers the stream in-webview dictation already holds (browserDictation.ts)
 * so the two are the same microphone, visibly in step. When the server is doing
 * the recording there is no such stream — nothing in this webview is listening —
 * so it opens its own, purely to have something to draw.
 */
function DictationWaveform({ state, seconds }: { state: MicState; seconds: number }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    if (state !== "recording") return;
    let stream: MediaStream | null = null;
    let audioContext: AudioContext | null = null;
    let frame: number | undefined;
    let cancelled = false;

    const draw = (analyser: AnalyserNode, samples: Uint8Array<ArrayBuffer>) => {
      const canvas = canvasRef.current;
      const context = canvas?.getContext("2d");
      if (canvas && context) {
        const { width, height } = canvas.getBoundingClientRect();
        const scale = window.devicePixelRatio || 1;
        if (
          canvas.width !== Math.round(width * scale) ||
          canvas.height !== Math.round(height * scale)
        ) {
          canvas.width = Math.round(width * scale);
          canvas.height = Math.round(height * scale);
        }
        context.setTransform(scale, 0, 0, scale, 0, 0);
        context.clearRect(0, 0, width, height);
        analyser.getByteTimeDomainData(samples);
        const bars = Math.max(30, Math.floor(width / 7));
        const step = samples.length / bars;
        const barWidth = Math.max(2, Math.min(3.5, width / bars - 3));
        context.fillStyle = "#747474";
        for (let bar = 0; bar < bars; bar += 1) {
          let energy = 0;
          const start = Math.floor(bar * step);
          const end = Math.min(samples.length, Math.floor((bar + 1) * step));
          for (let index = start; index < end; index += 1) {
            energy += Math.abs(samples[index] - 128);
          }
          const amplitude = energy / Math.max(1, end - start) / 128;
          const barHeight = Math.max(2, Math.min(height - 2, 2 + amplitude * height * 2.4));
          const x = bar * (width / bars) + (width / bars - barWidth) / 2;
          const y = (height - barHeight) / 2;
          context.globalAlpha = 0.4 + Math.min(0.6, amplitude * 2.5);
          context.fillRect(x, y, barWidth, barHeight);
        }
        context.globalAlpha = 1;
      }
      frame = window.requestAnimationFrame(() => draw(analyser, samples));
    };

    void (async () => {
      try {
        // Only a stream we opened ourselves gets stopped in the cleanup, so
        // borrowing the dictation one cannot cut the recording short.
        const borrowed = liveDictationStream();
        if (!borrowed) stream = await navigator.mediaDevices.getUserMedia({ audio: true });
        const source = borrowed ?? stream;
        if (cancelled || !source) {
          stream?.getTracks().forEach((track) => track.stop());
          return;
        }
        audioContext = new AudioContext();
        const analyser = audioContext.createAnalyser();
        analyser.fftSize = 512;
        analyser.smoothingTimeConstant = 0.78;
        audioContext.createMediaStreamSource(source).connect(analyser);
        draw(analyser, new Uint8Array(new ArrayBuffer(analyser.fftSize)));
      } catch {
        // No visualiser. The recording being made is untouched.
      }
    })();

    return () => {
      cancelled = true;
      if (frame !== undefined) window.cancelAnimationFrame(frame);
      stream?.getTracks().forEach((track) => track.stop());
      if (audioContext && audioContext.state !== "closed") {
        void audioContext.close().catch(() => undefined);
      }
    };
  }, [state]);

  const recording = state === "recording";
  return (
    <div
      className="dictation-waveform"
      role="status"
      aria-live="polite"
      aria-label={recording ? "Recording dictation" : "Transcribing dictation"}
    >
      <canvas ref={canvasRef} aria-hidden="true" />
      <span>
        {recording
          ? `Listening ${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`
          : "Transcribing"}
      </span>
    </div>
  );
}

/** One choice list inside the AI menu, in the prototype's row style. */
function MenuOptions({ options }: { options: SettingOption[] }) {
  return (
    <div className="model-menu__models">
      {options.map((option) => (
        <button
          className={`model-menu__item${option.selected ? " is-selected" : ""}`}
          type="button"
          key={option.value}
          onClick={option.onSelect}
        >
          {option.glyph}
          <span>{option.label}</span>
          {option.dot && <span className={`model-menu__dot is-${option.dot}`} aria-hidden="true" />}
          {option.tag && <em className="model-menu__tag">{option.tag}</em>}
        </button>
      ))}
    </div>
  );
}

/**
 * A labelled row that opens a flyout panel on hover/focus — the prototype's
 * Permission and Effort rows. `value` is cross-faded on change.
 */
function FlyoutRow({
  name,
  value,
  className,
  children,
}: {
  name: string;
  value: string;
  className: string;
  children: ReactNode;
}) {
  const [visibleValue, isSwapping] = useBlurSwap(value);
  return (
    <div
      className={className}
      tabIndex={0}
      role="button"
      aria-haspopup="menu"
      aria-label={`${name}: ${value}`}
      onClick={(event) => event.stopPropagation()}
    >
      <span className="model-menu__row-name">{name}</span>
      <span className={`model-menu__row-value model-menu__swap${isSwapping ? " is-swapping" : ""}`}>
        {visibleValue}
      </span>
      <HugeiconsIcon icon={ArrowDown01Icon} size={15} strokeWidth={1.8} />
      <div
        className="model-menu__row-panel glass-surface corner-smoothing"
        role="menu"
        aria-label={name}
        onClick={(event) => event.stopPropagation()}
      >
        {children}
      </div>
    </div>
  );
}

export function AlcoveComposer({
  text,
  placeholder,
  textareaRef,
  highlighted,
  backdropRef,
  onTextChange,
  onCursorChange,
  onPaste,
  onKeyDown,
  onSubmit,
  onInterrupt,
  canSend,
  running,
  queues,
  steersNow,
  attachments,
  attachFull,
  onAddFiles,
  onRemoveAttachment,
  agent,
  agentMark,
  agentOptions,
  showAgents,
  modelGroupLabel,
  modelOptions,
  effortOptions,
  sections,
  access,
  accessOptions,
  onAccessChange,
  mode,
  modeOptions,
  onModeChange,
  dictationAvailable,
  mic,
  micSeconds,
  onStartDictation,
  onStopDictation,
  voice,
  onVoice,
  voiceLive,
  children,
  banners,
  fileDragActive,
  dragHandlers,
}: AlcoveComposerProps) {
  const imageInputRef = useRef<HTMLInputElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const localTextarea = useRef<HTMLTextAreaElement | null>(null);
  const [attachmentMenuOpen, setAttachmentMenuOpen] = useState(false);
  const [modelMenuOpen, setModelMenuOpen] = useState(false);
  const [lines, setLines] = useState(1);
  // The prototype's send/mic hand-off, kept as its own state so the outgoing
  // glyph can animate out before the incoming one animates in.
  const [showStopping, setShowStopping] = useState(running);
  const [stopLeaving, setStopLeaving] = useState(false);
  const [sendExiting, setSendExiting] = useState(false);
  const [micArriving, setMicArriving] = useState(false);

  const dictating = mic === "recording" || mic === "transcribing";
  const hasContent = text.trim().length > 0 || attachments.length > 0;
  const micBusy = mic === "starting" || mic === "transcribing";

  // The prototype sizes the card off a fixed 17px line. Threadknot's composer
  // font is a user preference (COMPOSERPREFS_EVENT), so the multiline state is
  // measured from the box's own metrics instead of assumed from the text.
  useLayoutEffect(() => {
    const textarea = localTextarea.current;
    if (!textarea) {
      setLines(1);
      return;
    }
    const measure = () => {
      const lineHeight = parseFloat(getComputedStyle(textarea).lineHeight) || 17;
      setLines(Math.max(1, Math.round(textarea.clientHeight / lineHeight)));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(textarea);
    return () => observer.disconnect();
  }, [text]);

  useEffect(() => {
    if (running) {
      setStopLeaving(false);
      setMicArriving(false);
      if (showStopping) return;
      if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
        setShowStopping(true);
        return;
      }
      setSendExiting(true);
      const arriveTimer = window.setTimeout(() => {
        setShowStopping(true);
        setSendExiting(false);
      }, 110);
      return () => window.clearTimeout(arriveTimer);
    }
    if (!showStopping) return;
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      setShowStopping(false);
      return;
    }
    setStopLeaving(true);
    const leaveTimer = window.setTimeout(() => {
      setShowStopping(false);
      setStopLeaving(false);
      setSendExiting(false);
      setMicArriving(true);
    }, 150);
    const arriveTimer = window.setTimeout(() => setMicArriving(false), 360);
    return () => {
      window.clearTimeout(leaveTimer);
      window.clearTimeout(arriveTimer);
    };
  }, [running, showStopping]);

  const attachFrom = (files: FileList | null) => {
    if (files?.length) onAddFiles(Array.from(files));
  };
  const preserveInputFocus = (event: React.MouseEvent<HTMLButtonElement>) => {
    event.preventDefault();
  };

  const accessLabel =
    accessOptions.find((option) => option.id === access)?.title ?? "—";
  const modeLabel = modeOptions.find((option) => option.id === mode)?.title ?? "—";
  const selectedEffort = effortOptions.find((option) => option.selected);
  const effortIndex = Math.max(
    0,
    effortOptions.findIndex((option) => option.selected),
  );
  const effortSpan = Math.max(1, effortOptions.length - 1);
  const effortTrackRef = useRef<HTMLDivElement>(null);
  // The prototype tints the fill per provider — periwinkle for Codex,
  // terracotta for Claude. The panel is portalled out of the card, so the
  // colour rides on the slider inline rather than inheriting from it.
  const effortColor =
    agent === "claude" || agent === "claudex" ? "var(--alc-claude)" : "var(--alc-codex)";
  const pickEffortAt = (clientX: number) => {
    const track = effortTrackRef.current;
    if (!track || effortOptions.length === 0) return;
    const { left, width } = track.getBoundingClientRect();
    const fraction = Math.min(1, Math.max(0, (clientX - left) / width));
    effortOptions[Math.round(fraction * effortSpan)]?.onSelect();
  };

  // The primary button, in the prototype's own precedence: a dictation in
  // flight owns it, then a live turn, then a draft to send, then the mic.
  const primary = dictating
    ? "dictation"
    : showStopping && !queues
      ? "stop"
      : hasContent || sendExiting
        ? "send"
        : // An empty composer always shows the mic. It used to fall through to
          // a disabled send button when the server reported no dictation,
          // which left a dead blue pill sitting where the mic belongs; the
          // button now says so when pressed instead of vanishing.
          "mic";

  return (
    <div className="composer composer-anchor">
      {banners}
      <CornerSmoothing
        as="form"
        className={`composer-card glass-surface${lines > 1 ? " is-multiline" : ""}${
          attachments.length ? " has-images" : ""
        }${dictating ? ` is-dictating is-${mic}` : ""}${fileDragActive ? " file-drag" : ""}`}
        data-rounding-id="composer"
        smoothing={60}
        onSubmit={(event: React.FormEvent<HTMLFormElement>) => {
          event.preventDefault();
          if (dictating) return;
          if (hasContent) setSendExiting(true);
          onSubmit();
        }}
        {...dragHandlers}
      >
        <input
          ref={imageInputRef}
          className="attachment-menu__input"
          type="file"
          accept="image/png,image/jpeg,image/webp,image/gif,.png,.jpg,.jpeg,.webp,.gif"
          multiple
          onChange={(event) => {
            attachFrom(event.currentTarget.files);
            event.currentTarget.value = "";
          }}
        />
        {/* Deliberately unrestricted, as on the default skin: documents and
            images share one attachment path, and an image-only accept filter
            hides the PDFs and Word files agents read with their own tools. */}
        <input
          ref={fileInputRef}
          className="attachment-menu__input"
          type="file"
          multiple
          onChange={(event) => {
            attachFrom(event.currentTarget.files);
            event.currentTarget.value = "";
          }}
        />

        {children}

        {fileDragActive && <div className="composer-drop-hint">Drop files to attach</div>}

        {attachments.length > 0 && (
          <div
            className="composer-image-list"
            aria-label={`${attachments.length} file${attachments.length === 1 ? "" : "s"} attached`}
          >
            {attachments.map((attachment) => {
              const image = isImageAttachment(attachment.name, attachment.mimeType);
              return (
                <div
                  className={`composer-image${image ? "" : " composer-image--file"}`}
                  key={attachment.localId}
                >
                  {image ? (
                    <CornerSmoothing
                      as="img"
                      smoothing={60}
                      src={attachmentDataUrl(attachment)}
                      alt={attachment.name}
                    />
                  ) : (
                    <CornerSmoothing className="composer-image__file" smoothing={60}>
                      <HugeiconsIcon icon={FileUploadIcon} size={16} strokeWidth={1.7} />
                      <span>{attachment.name}</span>
                    </CornerSmoothing>
                  )}
                  <button
                    className="composer-image__remove glass-surface"
                    type="button"
                    aria-label={`Remove ${attachment.name}`}
                    onClick={() => onRemoveAttachment(attachment.localId)}
                  >
                    <HugeiconsIcon icon={Cancel01Icon} size={14} strokeWidth={2.2} />
                  </button>
                </div>
              );
            })}
          </div>
        )}

        {dictating ? (
          <DictationWaveform state={mic} seconds={micSeconds} />
        ) : (
          // Threadknot paints a highlighted copy of the draft behind a
          // transparent textarea; the prototype's plain field would drop
          // markdown highlighting, so the two layers come along.
          <div className="composer-md">
            <div ref={backdropRef} className="composer-md-backdrop" aria-hidden="true">
              {highlighted}
              {"\u200b"}
            </div>
            <textarea
              ref={(node) => {
                localTextarea.current = node;
                textareaRef(node);
              }}
              rows={1}
              value={text}
              placeholder={placeholder}
              aria-label="Message"
              autoCorrect="on"
              autoCapitalize="sentences"
              spellCheck={true}
              onChange={(event) => {
                onTextChange(event.target.value);
                onCursorChange(event.currentTarget);
              }}
              onClick={(event) => onCursorChange(event.currentTarget)}
              onSelect={(event) => onCursorChange(event.currentTarget)}
              onPaste={onPaste}
              onKeyDown={onKeyDown}
              onScroll={(event) => {
                const backdrop = event.currentTarget.previousElementSibling;
                if (backdrop instanceof HTMLElement) {
                  backdrop.scrollTop = event.currentTarget.scrollTop;
                }
              }}
            />
          </div>
        )}

        <div className="composer-actions">
          <Dropdown
            className="attachment-dropdown"
            menuClassName="attachment-menu"
            placement="top-left"
            tail={false}
            offset={6}
            onOpenChange={setAttachmentMenuOpen}
            trigger={
              <Tooltip content="Files & More" placement="top" disabled={attachmentMenuOpen}>
                <button
                  className="composer-icon"
                  data-motion-id="composer-add"
                  type="button"
                  aria-label="Files and more"
                  onMouseDown={preserveInputFocus}
                >
                  <HugeiconsIcon icon={Add01Icon} size={25} strokeWidth={1.65} />
                </button>
              </Tooltip>
            }
          >
            <div className="attachment-menu__items">
              <button
                type="button"
                className="attachment-menu__option"
                disabled={attachFull}
                onClick={() => imageInputRef.current?.click()}
              >
                <span className="attachment-menu__icon attachment-menu__icon--image">
                  <HugeiconsIcon icon={ImageUploadIcon} size={21} strokeWidth={1.7} />
                </span>
                <span>Insert image</span>
              </button>
              <button
                type="button"
                className="attachment-menu__option"
                disabled={attachFull}
                onClick={() => fileInputRef.current?.click()}
              >
                <span className="attachment-menu__icon attachment-menu__icon--file">
                  <HugeiconsIcon icon={FileUploadIcon} size={21} strokeWidth={1.7} />
                </span>
                <span>Insert file</span>
              </button>
              <button
                type="button"
                className="attachment-menu__option"
                onClick={() => {
                  onTextChange(text ? `${text.trimEnd()} Search the web for ` : "Search the web for ");
                  localTextarea.current?.focus();
                }}
              >
                <span className="attachment-menu__icon attachment-menu__icon--web">
                  <HugeiconsIcon icon={GlobalSearchIcon} size={21} strokeWidth={1.7} />
                </span>
                <span>Web search</span>
              </button>
            </div>
          </Dropdown>

          <div className="composer-trailing">
            {voice?.available && onVoice && (
              <Tooltip content="Voice conversation" placement="top">
                <button
                  className={`composer-icon voice-btn${voiceLive ? " live" : ""}`}
                  type="button"
                  aria-label="Voice conversation"
                  onMouseDown={preserveInputFocus}
                  onClick={onVoice}
                >
                  <VoiceWaveIcon size={23} />
                </button>
              </Tooltip>
            )}
          <div
            className={`composer-send-group${
              hasContent || showStopping || dictating ? " is-ready" : ""
            }`}
          >
            <Dropdown
              className="model-dropdown"
              menuClassName="model-menu"
              // How many fixed rows sit below the two scrolling lists. The
              // menu's height budget is the window minus these, and CSS has
              // no way to count them — a Codex thread carries Context and
              // Chrome on top of Mode, Permission and Effort, which is what
              // pushed the whole menu off the screen.
              menuStyle={
                {
                  "--model-menu-rows":
                    (mode ? 1 : 0) +
                    (access ? 1 : 0) +
                    (effortOptions.length > 0 ? 1 : 0) +
                    sections.length,
                } as CSSProperties
              }
              placement="top-right"
              tail={false}
              offset={6}
              onOpenChange={setModelMenuOpen}
              trigger={
                <Tooltip content="Model Settings" placement="top" disabled={modelMenuOpen}>
                  <button className="model-select" type="button" aria-label="Choose model settings">
                    <span className="model-select__mark">{agentMark}</span>
                  </button>
                </Tooltip>
              }
            >
              {showAgents && agentOptions.length > 0 && (
                <>
                  <span className="model-menu__label">Agent</span>
                  <div className="model-menu__providers">
                    {agentOptions.map((option) => (
                      <button
                        className={`model-menu__item${option.selected ? " is-selected" : ""}${
                          option.tag === "offline" ? " is-unavailable" : ""
                        }`}
                        type="button"
                        key={option.value}
                        onClick={option.onSelect}
                      >
                        {option.glyph}
                        <span>{option.label}</span>
                      </button>
                    ))}
                  </div>
                </>
              )}
              {modelOptions.length > 0 && (
                <>
                  <span className="model-menu__label">{modelGroupLabel}</span>
                  <MenuOptions options={modelOptions} />
                </>
              )}
              {mode && (
                <FlyoutRow name="Mode" value={modeLabel} className="model-menu__permission">
                  {modeOptions.map((option) => (
                    <button
                      className={`model-menu__permission-option${
                        mode === option.id ? " is-selected" : ""
                      }`}
                      type="button"
                      role="menuitemradio"
                      aria-checked={mode === option.id}
                      key={option.id}
                      onClick={() => onModeChange(option.id)}
                    >
                      {option.title}
                    </button>
                  ))}
                </FlyoutRow>
              )}
              {access && (
                <FlyoutRow name="Permission" value={accessLabel} className="model-menu__permission">
                  {accessOptions.map((option) => (
                    <button
                      className={`model-menu__permission-option${
                        access === option.id ? " is-selected" : ""
                      }`}
                      type="button"
                      role="menuitemradio"
                      aria-checked={access === option.id}
                      key={option.id}
                      onClick={() => onAccessChange(option.id)}
                    >
                      {option.title}
                    </button>
                  ))}
                </FlyoutRow>
              )}
              {effortOptions.length > 0 && (
                <div
                  className="model-menu__effort"
                  tabIndex={0}
                  onClick={(event) => event.stopPropagation()}
                >
                  <span className="model-menu__row-name">Effort</span>
                  <span className="model-menu__row-value">{selectedEffort?.label ?? "—"}</span>
                  <HugeiconsIcon icon={ArrowDown01Icon} size={15} strokeWidth={1.8} />
                  <div
                    className="model-menu__effort-panel glass-surface corner-smoothing"
                    onClick={(event) => event.stopPropagation()}
                  >
                    <div className="model-menu__effort-heading">
                      {agentMark}
                      <span>Effort</span>
                      <strong>{selectedEffort?.label ?? "—"}</strong>
                    </div>
                    {/* The prototype's slider has four fixed notches. A real
                        model advertises its own effort levels, so the track is
                        cut to however many this one has. The track itself takes
                        the pointer, as it does there; the notches are markings. */}
                    <div
                      ref={effortTrackRef}
                      className="model-menu__slider"
                      style={
                        {
                          "--effort-fill": `${(effortIndex / effortSpan) * 100}%`,
                          "--effort-position": `${(effortIndex / effortSpan) * 100}%`,
                          "--effort-color": effortColor,
                        } as CSSProperties
                      }
                      role="slider"
                      tabIndex={0}
                      aria-label="Reasoning effort"
                      aria-valuemin={1}
                      aria-valuemax={effortOptions.length}
                      aria-valuenow={effortIndex + 1}
                      aria-valuetext={selectedEffort?.label ?? ""}
                      onPointerDown={(event) => {
                        event.currentTarget.setPointerCapture(event.pointerId);
                        pickEffortAt(event.clientX);
                      }}
                      onPointerMove={(event) => {
                        if (event.currentTarget.hasPointerCapture(event.pointerId)) {
                          pickEffortAt(event.clientX);
                        }
                      }}
                      onKeyDown={(event) => {
                        if (event.key === "ArrowLeft" || event.key === "ArrowDown") {
                          event.preventDefault();
                          effortOptions[Math.max(0, effortIndex - 1)]?.onSelect();
                        }
                        if (event.key === "ArrowRight" || event.key === "ArrowUp") {
                          event.preventDefault();
                          effortOptions[
                            Math.min(effortOptions.length - 1, effortIndex + 1)
                          ]?.onSelect();
                        }
                      }}
                    >
                      <span className="model-menu__slider-fill" />
                      {effortOptions.map((option, notch) => (
                        <span
                          key={option.value}
                          className={`model-menu__slider-notch${
                            notch <= effortIndex ? " is-filled" : ""
                          }`}
                          style={
                            {
                              // The last marking is pulled inside the track's
                              // rounded cap, or it reads as detached from it.
                              "--notch-position":
                                notch === effortOptions.length - 1
                                  ? "calc(100% - 8px)"
                                  : `${(notch / effortSpan) * 100}%`,
                            } as CSSProperties
                          }
                          aria-hidden="true"
                        />
                      ))}
                      <span className="model-menu__slider-thumb" />
                    </div>
                  </div>
                </div>
              )}
              {sections.map((section) => (
                <div
                  className="model-menu__permission"
                  key={section.key}
                  tabIndex={section.disabled ? -1 : 0}
                  role="button"
                  aria-disabled={section.disabled}
                  onClick={(event) => {
                    event.stopPropagation();
                    if (!section.disabled) section.action?.();
                  }}
                >
                  <span className="model-menu__row-name">{section.label}</span>
                  <span className="model-menu__row-value">
                    {section.valueGlyph}
                    {section.valueLabel}
                  </span>
                  {section.options.length > 0 && (
                    <>
                      <HugeiconsIcon icon={ArrowDown01Icon} size={15} strokeWidth={1.8} />
                      <div
                        className="model-menu__row-panel glass-surface corner-smoothing"
                        role="menu"
                        aria-label={section.label}
                        onClick={(event) => event.stopPropagation()}
                      >
                        {section.options.map((option) => (
                          <button
                            className={`model-menu__permission-option${
                              option.selected ? " is-selected" : ""
                            }`}
                            type="button"
                            role="menuitemradio"
                            aria-checked={option.selected}
                            key={option.value}
                            onClick={option.onSelect}
                          >
                            {option.label}
                          </button>
                        ))}
                      </div>
                    </>
                  )}
                </div>
              ))}
            </Dropdown>

            {primary === "dictation" ? (
              <Tooltip
                className="stop-response-tooltip dictation-stop-tooltip"
                content={mic === "recording" ? "Stop and transcribe" : "Transcribing"}
                placement="top"
              >
                <button
                  className="send-control is-stopping dictation-stop"
                  type="button"
                  aria-label={mic === "recording" ? "Stop and transcribe" : "Transcribing recording"}
                  disabled={mic === "transcribing"}
                  onClick={onStopDictation}
                >
                  <span className="send-control__stop-mark" />
                </button>
              </Tooltip>
            ) : primary === "stop" ? (
              <Tooltip className="stop-response-tooltip" content="Interrupt turn" placement="top">
                <button
                  className={`send-control is-stopping${stopLeaving ? " is-leaving" : ""}`}
                  type="button"
                  aria-label="Interrupt turn"
                  onClick={onInterrupt}
                >
                  <span className="send-control__stop-mark" />
                </button>
              </Tooltip>
            ) : primary === "send" ? (
              <Tooltip
                content={running ? (steersNow ? "Send now" : "Queue message") : "Send"}
                placement="top"
              >
                <button
                  className={`send-control has-content${sendExiting ? " is-leaving" : ""}${
                    running && !steersNow ? " is-queueing" : ""
                  }`}
                  type="submit"
                  disabled={!canSend}
                  aria-label={
                    running ? (steersNow ? "Send message now" : "Queue message") : "Send message"
                  }
                >
                  {running && !steersNow ? (
                    <ClockIcon size={24} />
                  ) : (
                    <HugeiconsIcon
                      className="send-control__glyph send-control__glyph--send"
                      icon={ArrowUp02Icon}
                      size={28}
                      strokeWidth={3.2}
                    />
                  )}
                </button>
              </Tooltip>
            ) : (
              <Tooltip content="Dictate" placement="top">
                <button
                  className={`send-control${micArriving ? " is-arriving" : ""}${
                    micBusy ? " is-working" : ""
                  }`}
                  data-motion-id="composer-mic"
                  type="button"
                  disabled={micBusy}
                  aria-label="Dictate"
                  onClick={onStartDictation}
                >
                  <HugeiconsIcon
                    className="send-control__glyph send-control__glyph--mic"
                    icon={Mic01Icon}
                    size={25}
                    strokeWidth={1.6}
                  />
                </button>
              </Tooltip>
            )}
          </div>
          </div>
        </div>
      </CornerSmoothing>
    </div>
  );
}
