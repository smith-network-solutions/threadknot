import {
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import type { Project } from "../../lib/protocol";
import { termWsUrl } from "../../lib/discovery";
import { copyText } from "../../lib/format";
import { isNativeShell, readNativeClipboardText } from "../../lib/native";
import {
  APPEARANCE_EVENT,
  SKINPREFS_EVENT,
  TERMPREFS_EVENT,
  THEME_FAMILY_EVENT,
  clamp,
  getAppliedThemeFamily,
  getTermPrefs,
  monoFontEntry,
  monoFontStack,
  type TermPrefs,
  type ThemeFamily,
} from "../../lib/appearance";
import { ensureFontLoaded } from "../../lib/fonts";

/* Ctrl/cmd+wheel over the grid scales THIS terminal's font (the app-wide
 * zoom handler skips terminals, see lib/hotwheel.ts). Wider range than the
 * settings stepper on purpose. Persisted per terminal id. */
const WHEEL_FONT_MIN = 8;
const WHEEL_FONT_MAX = 28;
const WHEEL_NOTCH = 100;

interface TermCell {
  col: number;
  /** Absolute row in xterm's active buffer (including scrollback). */
  row: number;
}

interface TouchRange {
  anchor: TermCell;
  focus: TermCell;
}

interface SelectionDrag {
  pointerId: number;
  end: "new" | "anchor" | "focus";
  origin: TermCell;
  moved: boolean;
}

function fontKey(termId: string): string {
  return `threadknot.termfont.${termId}`;
}

function storedFontSize(termId: string): number | null {
  try {
    const raw = localStorage.getItem(fontKey(termId));
    if (!raw) return null;
    const n = Number(raw);
    return Number.isFinite(n) ? clamp(Math.round(n), WHEEL_FONT_MIN, WHEEL_FONT_MAX) : null;
  } catch {
    return null;
  }
}

// Dark palette harmonized with the app's :root vars (src/styles.css).
const DARK_THEME = {
  background: "#0b0d12",
  foreground: "#d8dde9",
  cursor: "#d9a35c",
  cursorAccent: "#0b0d12",
  selectionBackground: "rgba(217, 163, 92, 0.30)",
  black: "#10141d",
  red: "#e0655f",
  green: "#43c9a5",
  yellow: "#e5b567",
  blue: "#6fa8e8",
  magenta: "#b98ce8",
  cyan: "#43c9a5",
  white: "#d8dde9",
  brightBlack: "#5c6478",
  brightRed: "#f2857f",
  brightGreen: "#63e9c5",
  brightYellow: "#f2c98a",
  brightBlue: "#8fc0ff",
  brightMagenta: "#d0acff",
  brightCyan: "#63e9c5",
  brightWhite: "#ffffff",
};

// Light palette matched to the light-family ([data-family="light"]) vars.
const LIGHT_THEME = {
  background: "#ffffff",
  foreground: "#1b2233",
  cursor: "#b07322",
  cursorAccent: "#ffffff",
  selectionBackground: "rgba(176, 115, 34, 0.25)",
  black: "#1b2233",
  red: "#c23c37",
  green: "#0f8c68",
  yellow: "#9a6b12",
  blue: "#2f66bf",
  magenta: "#8a4fc0",
  cyan: "#0f8c8c",
  white: "#4a556b",
  brightBlack: "#55607a",
  brightRed: "#d8524c",
  brightGreen: "#12a67c",
  brightYellow: "#b07f28",
  brightBlue: "#3d6fc0",
  brightMagenta: "#a463d6",
  brightCyan: "#12a6a6",
  brightWhite: "#1b2233",
};

// The Retro-Tech skin's terminal: a single-gun green phosphor tube. The ANSI
// ramp keeps its hue ORDER (so colored output stays parseable) but every hue is
// pulled toward green/amber, which is all a real phosphor screen could ever
// show. Only the buffer colors live here; the scanline overlay and the glow are
// CSS (src/styles/terminal.css).
const PHOSPHOR_THEME = {
  background: "#030904",
  foreground: "#33ff33",
  cursor: "#33ff33",
  cursorAccent: "#030904",
  selectionBackground: "rgba(51,255,51,0.28)",
  black: "#0a140c",
  red: "#d98a4a",
  green: "#33ff33",
  yellow: "#d6dd4a",
  blue: "#4fbf8a",
  magenta: "#9ad46a",
  cyan: "#3fe0a8",
  white: "#c8ffc8",
  brightBlack: "#3d6b45",
  brightRed: "#ffb07a",
  brightGreen: "#7dff7d",
  brightYellow: "#eaf07a",
  brightBlue: "#7fe0b0",
  brightMagenta: "#b8f08a",
  brightCyan: "#6fffcf",
  brightWhite: "#eaffea",
};

/** Pick the xterm palette from the RESOLVED family, so a light-based custom
 *  theme (whose Appearance.theme still reads a dark id) gets the light one. */
function termTheme(family: ThemeFamily) {
  return family === "light" ? LIGHT_THEME : DARK_THEME;
}

/** Whether the phosphor tube is live right now. appearance.ts resolves the
 *  three conditions (the arcade theme applied, its terminal module on, and
 *  untouched terminal preferences: a hand-picked cursor or font is a deliberate
 *  choice a skin must not overrule) and writes the answer to data-phosphor, so
 *  the buffer palette here and the scanline CSS cannot disagree. */
function phosphorActive(): boolean {
  return document.documentElement.dataset.phosphor === "on";
}

/** Apply the palette + cursor look for the CURRENT theme/skin state. The
 *  phosphor cursor override is runtime only: nothing is written back to the
 *  stored prefs, so switching the module off restores the user's own cursor on
 *  the very next event. */
function applyTermLook(term: Terminal, family: ThemeFamily, prefs: TermPrefs): void {
  if (phosphorActive()) {
    term.options.theme = PHOSPHOR_THEME;
    term.options.cursorStyle = "block";
    term.options.cursorBlink = true;
    return;
  }
  term.options.theme = termTheme(family);
  term.options.cursorStyle = prefs.cursorStyle;
  term.options.cursorBlink = prefs.cursorBlink;
}

interface Props {
  project: Project;
  termId: string;
  http: { base: string; token: string };
  /** True when this terminal's tab is the visible one and the pane is active. */
  active: boolean;
  /** Owning machine when the project lives on a peer (socket splice). */
  machineId?: string;
}

/** A single xterm bound to one pty session over a resilient WebSocket. */
export function TerminalInstance({ project, termId, http, active, machineId }: Props) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const ctrlArmed = useRef(false);
  const ctrlBtnRef = useRef<HTMLButtonElement | null>(null);
  const closedByUs = useRef(false);
  const reconnectTimer = useRef<number | null>(null);
  const attempts = useRef(0);
  const initialized = useRef(false);
  /* xterm ANSWERS the queries it parses — cursor position (`ESC[6n` → `ESC[…R`),
   * device attributes (`ESC[c`), OSC 10/11 colour (→ `rgb:d8d8/dddd/e9e911`) —
   * and every answer goes into the pty as if it were typed. That is harmless
   * only while exactly one live terminal is doing the answering, which is not
   * our situation on two counts, so both are gated below:
   *   `replaying` — the server replays the scrollback on every attach, so the
   *     history's old queries get re-answered even though the programs that
   *     asked them are long gone. Reconnects (phone sleeping/waking) repeat it,
   *     which is why the junk KEEPS arriving at the prompt.
   *   `answering` — a pty fans out to every attached view; the server names one
   *     the responder so desktop + phone don't both answer live queries, with
   *     the second copy landing at the prompt. */
  const replaying = useRef(false);
  const expectReplay = useRef(false);
  const answering = useRef(true);
  const readingClipboardRef = useRef(false);
  const noticeTimer = useRef<number | null>(null);
  const touchSelectingRef = useRef(false);
  const touchRangeRef = useRef<TouchRange | null>(null);
  const selectionDrag = useRef<SelectionDrag | null>(null);
  const [readingClipboard, setReadingClipboard] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number; sel: string } | null>(null);
  const [termNotice, setTermNotice] = useState<string | null>(null);
  const [touchSelecting, setTouchSelecting] = useState(false);
  const [touchRange, setTouchRange] = useState<TouchRange | null>(null);
  // xterm owns the actual selection, so tick React when its viewport/selection
  // changes in order to keep our touch handles aligned with the painted cells.
  const [, setSelectionTick] = useState(0);

  // Lazy-init the terminal + socket once the tab first becomes active.
  useEffect(() => {
    if (!active || initialized.current || !hostRef.current) return;
    initialized.current = true;

    const prefs = getTermPrefs();
    // Decided once for the constructor so the very first paint is already the
    // right tube; every later change comes through the listeners below.
    const phosphor = phosphorActive();
    const term = new Terminal({
      scrollback: prefs.scrollback,
      fontSize: storedFontSize(termId) ?? prefs.fontSize,
      fontFamily: monoFontStack(prefs.fontFamily),
      cursorBlink: phosphor ? true : prefs.cursorBlink,
      cursorStyle: phosphor ? "block" : prefs.cursorStyle,
      allowProposedApi: true,
      theme: phosphor ? PHOSPHOR_THEME : termTheme(getAppliedThemeFamily()),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(hostRef.current);
    termRef.current = term;
    fitRef.current = fit;

    // xterm keeps its OWN selection (canvas-drawn, not a DOM selection), so the
    // webview's native Copy is dead and Ctrl+C would otherwise reach the pty as
    // SIGINT. Wire the terminal conventions explicitly: Ctrl/Cmd+Shift+C copies,
    // Ctrl+C copies when there's a selection (else interrupts), Ctrl/Cmd+Shift+V
    // and Cmd+V paste. Returning false stops xterm from forwarding the key.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      const primary = e.ctrlKey || e.metaKey;
      if (!primary || e.altKey) return true;
      const k = e.key.toLowerCase();
      if (k === "c") {
        const sel = term.getSelection();
        // Bare Ctrl+C with nothing selected must still interrupt the process.
        if (e.ctrlKey && !e.shiftKey && !e.metaKey && sel.length === 0) return true;
        if (sel.length > 0) {
          void copyText(sel);
          term.clearSelection();
        }
        return false;
      }
      if (k === "v" && (e.shiftKey || e.metaKey)) {
        void pasteFromClipboard();
        return false;
      }
      return true;
    });

    // Swallow the queries themselves when another view is the responder. Only
    // the query forms are intercepted; returning false hands the sequence back
    // to xterm, so a set-colour OSC (no trailing `?`) still applies here.
    const muted = () => !answering.current;
    term.parser.registerCsiHandler({ final: "n" }, muted); // DSR (cursor position)
    term.parser.registerCsiHandler({ prefix: "?", final: "n" }, muted); // DECDSR
    term.parser.registerCsiHandler({ final: "c" }, muted); // DA1
    term.parser.registerCsiHandler({ prefix: ">", final: "c" }, muted); // DA2
    for (const osc of [4, 10, 11, 12]) {
      term.parser.registerOscHandler(osc, (data) => data.endsWith("?") && muted());
    }

    const send = (data: string) => {
      // Everything xterm emits mid-replay is an answer to a question from the
      // past — dropping it is what keeps replayed history out of the pty.
      if (replaying.current) return;
      const ws = wsRef.current;
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "input", data }));
      }
    };

    term.onData((data) => {
      if (ctrlArmed.current && data.length === 1) {
        const code = data.toUpperCase().charCodeAt(0) & 0x1f;
        data = String.fromCharCode(code);
        setCtrl(false);
      }
      send(data);
    });
    term.onScroll(() => {
      if (touchSelectingRef.current) setSelectionTick((tick) => tick + 1);
    });

    const setCtrl = (on: boolean) => {
      ctrlArmed.current = on;
      ctrlBtnRef.current?.classList.toggle("on", on);
    };
    // expose for the accessory row handlers
    (term as unknown as { _setCtrl?: (on: boolean) => void })._setCtrl = setCtrl;

    // Ctrl/cmd+wheel: scale this terminal's font. stopPropagation keeps the
    // app-wide zoom handler (window-level) out of it; accumulate deltas so a
    // trackpad pinch steps at the same rate as a detented wheel notch.
    let wheelAcc = 0;
    hostRef.current.addEventListener(
      "wheel",
      (e: WheelEvent) => {
        if (!e.ctrlKey && !e.metaKey) return;
        e.preventDefault();
        e.stopPropagation();
        wheelAcc += e.deltaY * (e.deltaMode === 1 ? 33 : 1);
        const steps = Math.trunc(wheelAcc / WHEEL_NOTCH);
        if (steps === 0) return;
        wheelAcc -= steps * WHEEL_NOTCH;
        const cur = term.options.fontSize ?? prefs.fontSize;
        const next = clamp(cur - steps, WHEEL_FONT_MIN, WHEEL_FONT_MAX);
        if (next === cur) return;
        term.options.fontSize = next;
        safeFit();
        try {
          localStorage.setItem(fontKey(termId), String(next));
        } catch {
          /* persistence is a convenience only */
        }
      },
      { passive: false },
    );

    safeFit();
    // The constructor measured against whatever was loaded at open; if the mono
    // family is a still-fetching webfont, re-fit once it is usable so the very
    // first paint isn't stuck on fallback cell metrics.
    void ensureFontLoaded(monoFontEntry(prefs.fontFamily)).then(() => {
      if (termRef.current === term) safeFit();
    });
    connect();
    // NOTE: no cleanup here. This effect re-runs when `active` toggles, and an
    // inline cleanup would dispose the terminal every time the tab is hidden —
    // but `initialized` is never reset, so it would never be recreated (black
    // terminal on tab switch). Teardown lives in the unmount-only effect below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  // Dispose the terminal + socket only when this instance truly unmounts
  // (tab deleted or the pane's project changed) — never on a tab switch.
  useEffect(() => {
    return () => {
      closedByUs.current = true;
      if (reconnectTimer.current != null) window.clearTimeout(reconnectTimer.current);
      wsRef.current?.close();
      wsRef.current = null;
      termRef.current?.dispose();
      termRef.current = null;
      if (noticeTimer.current != null) window.clearTimeout(noticeTimer.current);
    };
  }, []);

  // Live-apply terminal + theme + skin preference changes from the settings
  // popover.
  useEffect(() => {
    // Re-decide palette + cursor from whatever the DOM says now. Callers pass
    // the event's own detail when they have it (it is authoritative before the
    // module-level caches settle), otherwise we read the stored values.
    function refreshLook(family?: ThemeFamily, prefs?: TermPrefs) {
      const term = termRef.current;
      if (!term) return;
      applyTermLook(term, family ?? getAppliedThemeFamily(), prefs ?? getTermPrefs());
    }
    function onTermPrefs(e: Event) {
      const term = termRef.current;
      if (!term) return;
      const p = (e as CustomEvent<TermPrefs>).detail;
      // A global font change from Settings wins: apply it and drop this
      // terminal's ctrl+wheel override so the stepper stays predictable.
      term.options.fontSize = p.fontSize;
      try {
        localStorage.removeItem(fontKey(termId));
      } catch {
        /* persistence is a convenience only */
      }
      term.options.cursorStyle = p.cursorStyle;
      term.options.cursorBlink = p.cursorBlink;
      term.options.scrollback = p.scrollback;
      // A font change re-measures the cell, so fit after applying it. xterm
      // measures against whatever is loaded NOW, though, so a webfont that is
      // still fetching yields fallback-sized cells until a resize. Await the
      // family becoming usable, then re-apply + re-fit so metrics self-correct
      // (guarded by the live term ref: the pref may change again meanwhile).
      term.options.fontFamily = monoFontStack(p.fontFamily);
      safeFit();
      void ensureFontLoaded(monoFontEntry(p.fontFamily)).then(() => {
        const live = termRef.current;
        if (!live || live.options.fontFamily !== monoFontStack(p.fontFamily)) return;
        live.options.fontFamily = monoFontStack(p.fontFamily);
        safeFit();
      });
      // The prefs just moved, which is exactly what can switch the phosphor
      // tube off (or back on): re-decide AFTER the user's own values landed.
      refreshLook(undefined, p);
    }
    function onFamily(e: Event) {
      // Resolved family covers presets, a custom theme's base, and live
      // studio previews alike, unlike Appearance.theme.
      refreshLook((e as CustomEvent<ThemeFamily>).detail);
    }
    function onSkinOrAppearance() {
      refreshLook();
    }
    window.addEventListener(TERMPREFS_EVENT, onTermPrefs);
    window.addEventListener(THEME_FAMILY_EVENT, onFamily);
    // A skin toggle and a theme swap within one family both leave the family
    // event silent, so the palette has to be re-decided on these too.
    window.addEventListener(SKINPREFS_EVENT, onSkinOrAppearance);
    window.addEventListener(APPEARANCE_EVENT, onSkinOrAppearance);
    return () => {
      window.removeEventListener(TERMPREFS_EVENT, onTermPrefs);
      window.removeEventListener(THEME_FAMILY_EVENT, onFamily);
      window.removeEventListener(SKINPREFS_EVENT, onSkinOrAppearance);
      window.removeEventListener(APPEARANCE_EVENT, onSkinOrAppearance);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function safeFit() {
    const term = termRef.current;
    const fit = fitRef.current;
    if (!term || !fit || !hostRef.current) return;
    if (hostRef.current.clientWidth === 0 || hostRef.current.clientHeight === 0) return;
    try {
      fit.fit();
    } catch {
      return;
    }
    if (touchSelectingRef.current) setSelectionTick((tick) => tick + 1);
    const ws = wsRef.current;
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
    }
  }

  function connect() {
    const term = termRef.current;
    if (!term) return;
    const url = termWsUrl(http, project.id, termId, { cols: term.cols, rows: term.rows }, machineId);
    const ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";
    wsRef.current = ws;
    // On (re)connect, reset so a replayed scrollback isn't duplicated.
    if (attempts.current > 0) term.reset();
    // A socket that died mid-replay must not leave input muted for good.
    replaying.current = false;
    expectReplay.current = false;

    ws.onopen = () => {
      attempts.current = 0;
      safeFit();
    };
    ws.onmessage = (ev) => {
      if (typeof ev.data === "string") {
        try {
          const msg = JSON.parse(ev.data);
          if (msg.type === "replay") {
            // The next binary frame is history, not live output.
            expectReplay.current = true;
            return;
          }
          if (msg.type === "role") {
            answering.current = msg.responder !== false;
            return;
          }
          if (msg.type === "exit") {
            closedByUs.current = true;
            term.write(
              `\r\n\x1b[2m[process exited${
                msg.code != null ? ` (${msg.code})` : ""
              }] — press Enter to restart\x1b[0m\r\n`,
            );
            armRestart();
          }
        } catch {
          /* ignore malformed control frame */
        }
        return;
      }
      const bytes = new Uint8Array(ev.data as ArrayBuffer);
      if (expectReplay.current) {
        expectReplay.current = false;
        // Mute until the parser has chewed through the whole replay — the
        // callback fires once this write is fully processed.
        replaying.current = true;
        term.write(bytes, () => {
          replaying.current = false;
        });
        return;
      }
      term.write(bytes);
    };
    ws.onclose = () => {
      if (closedByUs.current) return;
      scheduleReconnect();
    };
    ws.onerror = () => {
      ws.close();
    };
  }

  function scheduleReconnect() {
    if (document.hidden || attempts.current >= 30) return;
    attempts.current += 1;
    const delay = Math.min(1000 * attempts.current, 5000);
    reconnectTimer.current = window.setTimeout(() => connect(), delay);
  }

  // A dead session was removed server-side; Enter re-attaches with a fresh spawn.
  function armRestart() {
    const term = termRef.current;
    if (!term) return;
    const disp = term.onData((d) => {
      if (d === "\r" || d === "\n") {
        disp.dispose();
        closedByUs.current = false;
        attempts.current = 0;
        term.reset();
        connect();
      }
    });
  }

  /** Ask the server to make this view the query responder (see `answering`). */
  function claimResponder() {
    const ws = wsRef.current;
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: "claim" }));
  }

  // Re-fit whenever the pane is resized or this tab becomes visible.
  useEffect(() => {
    if (!active) return;
    const host = hostRef.current;
    if (!host) return;
    const ro = new ResizeObserver(() => safeFit());
    ro.observe(host);
    // becoming active: fit + focus on next frame (layout settled)
    const raf = requestAnimationFrame(() => {
      safeFit();
      termRef.current?.focus();
      claimResponder();
    });
    // Coming back from a backgrounded tab, take the role back: a frozen tab
    // keeps its socket but stops answering.
    const onVisible = () => {
      if (!document.hidden) claimResponder();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      ro.disconnect();
      cancelAnimationFrame(raf);
      document.removeEventListener("visibilitychange", onVisible);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);

  function key(data: string) {
    const ws = wsRef.current;
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "input", data }));
    }
    termRef.current?.focus();
  }

  function toggleCtrl() {
    const term = termRef.current as unknown as { _setCtrl?: (on: boolean) => void } | null;
    term?._setCtrl?.(!ctrlArmed.current);
    termRef.current?.focus();
  }

  function pasteIntoTerminal(text: string) {
    if (!text) return;
    // xterm.paste preserves terminal bracketed-paste semantics and reaches the
    // same onData socket path as a keyboard paste.
    termRef.current?.paste(text);
    termRef.current?.focus();
  }

  function showTermNotice(message: string) {
    if (noticeTimer.current != null) window.clearTimeout(noticeTimer.current);
    setTermNotice(message);
    noticeTimer.current = window.setTimeout(() => {
      setTermNotice(null);
      noticeTimer.current = null;
    }, 3200);
  }

  function hasTouchPrimaryInput() {
    return window.matchMedia?.("(hover: none) and (pointer: coarse)").matches === true;
  }

  async function pasteFromClipboard() {
    if (readingClipboardRef.current) return;
    readingClipboardRef.current = true;
    setReadingClipboard(true);
    let text: string | null = null;
    try {
      if (isNativeShell()) {
        text = await readNativeClipboardText();
      } else if (window.isSecureContext && navigator.clipboard?.readText) {
        text = await navigator.clipboard.readText();
      }
    } catch {
      // A normal paste event into xterm still works. Programmatic clipboard
      // reads are the part browsers block on HTTP or after a permission denial.
    } finally {
      readingClipboardRef.current = false;
      setReadingClipboard(false);
    }

    if (text) {
      pasteIntoTerminal(text);
      return;
    }
    // Do not open a second editing box. xterm already receives ordinary paste
    // events through its focused textarea; on touch browsers its own context
    // menu handler moves that textarea under the press so the OS Paste command
    // can target it. We merely explain the gesture when async clipboard access
    // is unavailable.
    termRef.current?.focus();
    showTermNotice(
      hasTouchPrimaryInput()
        ? "Touch and hold in the terminal, then choose Paste."
        : "Use Ctrl+Shift+V (or Cmd+V) to paste into the terminal.",
    );
  }

  function openContextMenu(e: ReactMouseEvent) {
    const term = termRef.current;
    if (!term) return;
    // xterm's native context-menu path positions and focuses its hidden input,
    // which is exactly what a phone needs for the OS Paste command. The old
    // preventDefault here suppressed that path and forced the manual textarea.
    if (hasTouchPrimaryInput() && !touchSelecting) {
      setMenu(null);
      return;
    }
    // Replace the webview's (broken, always-grayed) native menu with our own
    // on desktop, where right-click and xterm's mouse selection work well.
    e.preventDefault();
    setMenu({ x: e.clientX, y: e.clientY, sel: term.getSelection() });
  }

  function cellFromClientPoint(clientX: number, clientY: number): TermCell | null {
    const term = termRef.current;
    const screen = term?.element?.querySelector<HTMLElement>(".xterm-screen");
    if (!term || !screen) return null;
    const rect = screen.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return null;
    const col = clamp(Math.floor(((clientX - rect.left) / rect.width) * term.cols), 0, term.cols - 1);
    const viewportRow = clamp(
      Math.floor(((clientY - rect.top) / rect.height) * term.rows),
      0,
      term.rows - 1,
    );
    return { col, row: term.buffer.active.viewportY + viewportRow };
  }

  function applyTouchRange(anchor: TermCell, focus: TermCell) {
    const term = termRef.current;
    if (!term) return;
    const anchorIndex = anchor.row * term.cols + anchor.col;
    const focusIndex = focus.row * term.cols + focus.col;
    const first = Math.min(anchorIndex, focusIndex);
    const last = Math.max(anchorIndex, focusIndex);
    const range = { anchor, focus };
    touchRangeRef.current = range;
    setTouchRange(range);
    term.select(first % term.cols, Math.floor(first / term.cols), last - first + 1);
    setSelectionTick((tick) => tick + 1);
  }

  function wordAtCell(cell: TermCell): TouchRange {
    const term = termRef.current;
    const line = term?.buffer.active.getLine(cell.row);
    if (!term || !line) return { anchor: cell, focus: cell };
    const chars = (col: number) => line.getCell(col)?.getChars() ?? "";
    const hit = chars(cell.col);
    if (!hit || /\s/u.test(hit)) return { anchor: cell, focus: cell };
    let first = cell.col;
    let last = cell.col;
    while (first > 0) {
      const value = chars(first - 1);
      if (!value || /\s/u.test(value)) break;
      first -= 1;
    }
    while (last + 1 < term.cols) {
      const value = chars(last + 1);
      if (!value || /\s/u.test(value)) break;
      last += 1;
    }
    return {
      anchor: { col: first, row: cell.row },
      focus: { col: last, row: cell.row },
    };
  }

  function beginTouchSelection(e: ReactPointerEvent<HTMLDivElement>) {
    if (!touchSelecting) return;
    const cell = cellFromClientPoint(e.clientX, e.clientY);
    if (!cell) return;
    e.preventDefault();
    e.stopPropagation();
    e.currentTarget.setPointerCapture(e.pointerId);
    selectionDrag.current = {
      pointerId: e.pointerId,
      end: "new",
      origin: cell,
      moved: false,
    };
    applyTouchRange(cell, cell);
  }

  function moveTouchSelection(e: ReactPointerEvent<HTMLElement>) {
    const drag = selectionDrag.current;
    if (!drag || drag.pointerId !== e.pointerId) return;
    const cell = cellFromClientPoint(e.clientX, e.clientY);
    const range = touchRangeRef.current;
    if (!cell || !range) return;
    e.preventDefault();
    if (cell.col !== drag.origin.col || cell.row !== drag.origin.row) drag.moved = true;
    if (drag.end === "anchor") applyTouchRange(cell, range.focus);
    else if (drag.end === "focus") applyTouchRange(range.anchor, cell);
    else applyTouchRange(range.anchor, cell);
  }

  function finishTouchSelection(e: ReactPointerEvent<HTMLElement>) {
    const drag = selectionDrag.current;
    if (!drag || drag.pointerId !== e.pointerId) return;
    e.preventDefault();
    if (drag.end === "new" && !drag.moved) {
      const word = wordAtCell(drag.origin);
      applyTouchRange(word.anchor, word.focus);
    }
    selectionDrag.current = null;
    try {
      e.currentTarget.releasePointerCapture(e.pointerId);
    } catch {
      /* capture can already be gone after a browser-level gesture */
    }
  }

  function beginHandleDrag(end: "anchor" | "focus", e: ReactPointerEvent<HTMLButtonElement>) {
    const range = touchRangeRef.current;
    if (!range) return;
    e.preventDefault();
    e.stopPropagation();
    e.currentTarget.setPointerCapture(e.pointerId);
    selectionDrag.current = {
      pointerId: e.pointerId,
      end,
      origin: end === "anchor" ? range.anchor : range.focus,
      moved: false,
    };
  }

  function handleStyle(cell: TermCell, trailing: boolean): CSSProperties | null {
    const term = termRef.current;
    const host = hostRef.current;
    const screen = term?.element?.querySelector<HTMLElement>(".xterm-screen");
    const instance = host?.parentElement;
    if (!term || !screen || !instance) return null;
    const viewportRow = cell.row - term.buffer.active.viewportY;
    if (viewportRow < 0 || viewportRow >= term.rows) return null;
    const screenRect = screen.getBoundingClientRect();
    const instanceRect = instance.getBoundingClientRect();
    const cellWidth = screenRect.width / term.cols;
    const cellHeight = screenRect.height / term.rows;
    return {
      left:
        screenRect.left - instanceRect.left +
        (cell.col + (trailing ? 1 : 0)) * cellWidth,
      top: screenRect.top - instanceRect.top + (viewportRow + 1) * cellHeight,
    };
  }

  function stopTouchSelection(clear = true) {
    selectionDrag.current = null;
    touchSelectingRef.current = false;
    touchRangeRef.current = null;
    setTouchRange(null);
    setTouchSelecting(false);
    if (clear) termRef.current?.clearSelection();
    setSelectionTick((tick) => tick + 1);
    termRef.current?.focus();
  }

  function toggleTouchSelection() {
    if (touchSelecting) {
      stopTouchSelection();
      return;
    }
    setMenu(null);
    touchSelectingRef.current = true;
    setTouchSelecting(true);
    termRef.current?.clearSelection();
    setSelectionTick((tick) => tick + 1);
  }

  async function copyTouchSelection() {
    const text = termRef.current?.getSelection() ?? "";
    if (!text) return;
    const copied = await copyText(text);
    stopTouchSelection();
    showTermNotice(copied ? "Copied terminal selection." : "Couldn’t copy this selection.");
  }

  function selectAllForTouch() {
    termRef.current?.selectAll();
    touchRangeRef.current = null;
    setTouchRange(null);
    setSelectionTick((tick) => tick + 1);
  }

  // Dismiss the context menu on any click/scroll/resize/Escape elsewhere.
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMenu(null);
    };
    window.addEventListener("click", close);
    window.addEventListener("resize", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("resize", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  const selectedText = touchSelecting ? (termRef.current?.getSelection() ?? "") : "";
  const rangeRunsForward = touchRange
    ? touchRange.anchor.row < touchRange.focus.row ||
      (touchRange.anchor.row === touchRange.focus.row &&
        touchRange.anchor.col <= touchRange.focus.col)
    : true;
  const anchorStyle = touchRange ? handleStyle(touchRange.anchor, !rangeRunsForward) : null;
  const focusStyle = touchRange ? handleStyle(touchRange.focus, rangeRunsForward) : null;

  return (
    <div className="term-instance" hidden={!active}>
      <div
        className={`term-host${touchSelecting ? " selecting" : ""}`}
        ref={hostRef}
        onContextMenu={openContextMenu}
        onPointerDown={beginTouchSelection}
        onPointerMove={moveTouchSelection}
        onPointerUp={finishTouchSelection}
        onPointerCancel={finishTouchSelection}
      />
      {menu && (
        <div className="term-ctx" style={{ left: menu.x, top: menu.y }} role="menu">
          <button
            type="button"
            role="menuitem"
            disabled={!menu.sel}
            onClick={() => {
              if (menu.sel) {
                void copyText(menu.sel);
                termRef.current?.clearSelection();
              }
              setMenu(null);
            }}
          >
            Copy
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setMenu(null);
              void pasteFromClipboard();
            }}
          >
            Paste
          </button>
        </div>
      )}
      {termNotice && (
        <div className="term-notice" role="status">
          {termNotice}
        </div>
      )}
      {touchSelecting && (
        <div
          className="term-selection-bar"
          role="toolbar"
          aria-label="Terminal selection"
          onMouseDown={(event) => event.preventDefault()}
        >
          <span>{selectedText ? `${selectedText.length} selected` : "Drag across text"}</span>
          <button type="button" disabled={!selectedText} onClick={() => void copyTouchSelection()}>
            Copy
          </button>
          <button type="button" onClick={selectAllForTouch}>All</button>
          <button type="button" onClick={() => stopTouchSelection()}>Done</button>
        </div>
      )}
      {touchSelecting && anchorStyle && (
        <button
          type="button"
          className="term-selection-handle"
          style={anchorStyle}
          aria-label="Move selection start"
          onPointerDown={(event) => beginHandleDrag("anchor", event)}
          onPointerMove={moveTouchSelection}
          onPointerUp={finishTouchSelection}
          onPointerCancel={finishTouchSelection}
        />
      )}
      {touchSelecting && focusStyle && (
        <button
          type="button"
          className="term-selection-handle end"
          style={focusStyle}
          aria-label="Move selection end"
          onPointerDown={(event) => beginHandleDrag("focus", event)}
          onPointerMove={moveTouchSelection}
          onPointerUp={finishTouchSelection}
          onPointerCancel={finishTouchSelection}
        />
      )}
      {/* Tapping an accessory key must NOT move focus off the terminal. A
       * blur/refocus pair emits DECSET-1004 focus reports (\x1b[O, \x1b[I) into
       * the pty whenever an app left that mode on, and node's readline names
       * those sequences `undefined` — so prompt libraries (eas/expo's `prompts`)
       * type the literal text "undefined" into their filter on every keypress.
       * Cancelling mousedown keeps focus (and the soft keyboard) on the term. */}
      <div
        className="term-keys"
        role="toolbar"
        aria-label="Terminal keys"
        onMouseDown={(e) => e.preventDefault()}
      >
        <button type="button" onClick={() => key("\x1b")}>Esc</button>
        <button type="button" onClick={() => key("\t")}>Tab</button>
        <button type="button" ref={ctrlBtnRef} onClick={toggleCtrl}>Ctrl</button>
        <button
          type="button"
          className={touchSelecting ? "on" : undefined}
          aria-pressed={touchSelecting}
          onClick={toggleTouchSelection}
        >
          Select
        </button>
        <button
          type="button"
          aria-label="Paste into terminal"
          disabled={readingClipboard}
          onClick={() => void pasteFromClipboard()}
        >
          {readingClipboard ? "Reading…" : "Paste"}
        </button>
        <button type="button" onClick={() => key("\x1b[A")} aria-label="Up">↑</button>
        <button type="button" onClick={() => key("\x1b[B")} aria-label="Down">↓</button>
        <button type="button" onClick={() => key("\x1b[D")} aria-label="Left">←</button>
        <button type="button" onClick={() => key("\x1b[C")} aria-label="Right">→</button>
        <button type="button" className="term-key-danger" onClick={() => key("\x03")}>^C</button>
        <button
          type="button"
          className="term-key-danger"
          aria-label="Kill shell"
          title="Kill the shell process (the tab stays — press Enter to restart)"
          onClick={() => {
            // Kill the shell but keep the tab: the server broadcasts `exit`, the
            // pane shows a restart prompt, and Enter re-spawns a fresh shell.
            // Permanent removal is the tab's × button (term.delete).
            wsRef.current?.send(JSON.stringify({ type: "kill" }));
          }}
        >
          Kill
        </button>
      </div>
    </div>
  );
}
