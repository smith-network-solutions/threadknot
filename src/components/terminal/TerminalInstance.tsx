import { useEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import type { Project } from "../../lib/protocol";
import { termWsUrl } from "../../lib/discovery";
import { copyText } from "../../lib/format";
import { isNativeShell, readNativeClipboardText } from "../../lib/native";
import {
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

/** Pick the xterm palette from the RESOLVED family, so a light-based custom
 *  theme (whose Appearance.theme still reads a dark id) gets the light one. */
function termTheme(family: ThemeFamily) {
  return family === "light" ? LIGHT_THEME : DARK_THEME;
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
  const [pasteOpen, setPasteOpen] = useState(false);
  const [pasteText, setPasteText] = useState("");
  const [readingClipboard, setReadingClipboard] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number; sel: string } | null>(null);

  // Lazy-init the terminal + socket once the tab first becomes active.
  useEffect(() => {
    if (!active || initialized.current || !hostRef.current) return;
    initialized.current = true;

    const prefs = getTermPrefs();
    const term = new Terminal({
      scrollback: prefs.scrollback,
      fontSize: storedFontSize(termId) ?? prefs.fontSize,
      fontFamily: monoFontStack(prefs.fontFamily),
      cursorBlink: prefs.cursorBlink,
      cursorStyle: prefs.cursorStyle,
      allowProposedApi: true,
      theme: termTheme(getAppliedThemeFamily()),
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
    };
  }, []);

  // Live-apply terminal + theme preference changes from the settings popover.
  useEffect(() => {
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
    }
    function onFamily(e: Event) {
      const term = termRef.current;
      if (!term) return;
      // Resolved family covers presets, a custom theme's base, and live
      // studio previews alike — unlike Appearance.theme.
      term.options.theme = termTheme((e as CustomEvent<ThemeFamily>).detail);
    }
    window.addEventListener(TERMPREFS_EVENT, onTermPrefs);
    window.addEventListener(THEME_FAMILY_EVENT, onFamily);
    return () => {
      window.removeEventListener(TERMPREFS_EVENT, onTermPrefs);
      window.removeEventListener(THEME_FAMILY_EVENT, onFamily);
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

  async function pasteFromClipboard() {
    if (readingClipboard) return;
    setReadingClipboard(true);
    let text: string | null = null;
    try {
      if (isNativeShell()) {
        text = await readNativeClipboardText();
      } else if (window.isSecureContext && navigator.clipboard?.readText) {
        text = await navigator.clipboard.readText();
      }
    } catch {
      // HTTP phone browsers and denied clipboard permissions use the manual
      // paste sheet below.
    } finally {
      setReadingClipboard(false);
    }

    if (text) {
      pasteIntoTerminal(text);
      return;
    }
    setPasteText("");
    setPasteOpen(true);
  }

  function submitManualPaste() {
    if (!pasteText) return;
    pasteIntoTerminal(pasteText);
    setPasteText("");
    setPasteOpen(false);
  }

  function openContextMenu(e: ReactMouseEvent) {
    const term = termRef.current;
    if (!term) return;
    // Replace the webview's (broken, always-grayed) native menu with our own
    // that reads xterm's selection.
    e.preventDefault();
    setMenu({ x: e.clientX, y: e.clientY, sel: term.getSelection() });
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

  return (
    <div className="term-instance" hidden={!active}>
      <div className="term-host" ref={hostRef} onContextMenu={openContextMenu} />
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
      {pasteOpen && (
        <form
          className="term-paste-sheet"
          aria-label="Paste text into terminal"
          onSubmit={(event) => {
            event.preventDefault();
            submitManualPaste();
          }}
        >
          <label htmlFor={`term-paste-${termId}`}>
            Paste text here
            <span>Clipboard access is blocked, so use your phone’s Paste command.</span>
          </label>
          <textarea
            id={`term-paste-${termId}`}
            value={pasteText}
            autoFocus
            rows={3}
            placeholder="Touch and hold here, then tap Paste"
            onChange={(event) => setPasteText(event.currentTarget.value)}
          />
          <div className="term-paste-actions">
            <button
              type="button"
              onClick={() => {
                setPasteText("");
                setPasteOpen(false);
              }}
            >
              Cancel
            </button>
            <button type="submit" className="primary" disabled={!pasteText}>
              Send to terminal
            </button>
          </div>
        </form>
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
