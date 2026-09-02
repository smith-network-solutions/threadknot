import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import type { VoiceSessionPhase, VoiceStateFrame } from "../lib/protocol";
import { useStore } from "../state/store";

/** Human line under the knot for each phase. */
const PHASE_WORDS: Record<VoiceSessionPhase, string> = {
  idle: "ready",
  listening: "listening",
  transcribing: "hearing you out",
  thinking: "thinking",
  synthesizing: "finding its voice",
  speaking: "speaking",
  interrupted: "go ahead",
  error: "something snagged",
};

/** Per-phase animation targets the canvas lerps toward. */
interface KnotParams {
  spin: number;
  amplitude: number;
  glow: number;
  /** 0 = teal, 1 = brass, 2 = brass-hi, 3 = red. */
  palette: number;
}

const PHASE_PARAMS: Record<VoiceSessionPhase, KnotParams> = {
  idle: { spin: 0.1, amplitude: 0.05, glow: 0.25, palette: 0 },
  listening: { spin: 0.25, amplitude: 0.12, glow: 0.55, palette: 0 },
  transcribing: { spin: 0.9, amplitude: 0.18, glow: 0.6, palette: 1 },
  thinking: { spin: 0.35, amplitude: 0.1, glow: 0.5, palette: 1 },
  synthesizing: { spin: 1.3, amplitude: 0.2, glow: 0.75, palette: 2 },
  speaking: { spin: 0.5, amplitude: 0.32, glow: 0.85, palette: 2 },
  interrupted: { spin: 0.15, amplitude: 0.08, glow: 0.9, palette: 3 },
  error: { spin: 0, amplitude: 0, glow: 0.7, palette: 3 },
};

/**
 * The Threadknot knot: three interwoven rope strands, brass with teal
 * running-lights, breathing with the session. One rAF loop on one canvas —
 * cheap, GPU-composited, and honest about prefers-reduced-motion (a static
 * frame per phase instead of the loop).
 */
function VoiceKnotCanvas({
  frame,
  size = 420,
}: {
  frame: VoiceStateFrame | null;
  size?: number;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const live = useRef({ frame });
  live.current.frame = frame;

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const css = getComputedStyle(document.documentElement);
    const palette = [
      css.getPropertyValue("--teal").trim() || "#43c9a5",
      css.getPropertyValue("--brass").trim() || "#d9a35c",
      css.getPropertyValue("--brass-hi").trim() || "#f2c98a",
      css.getPropertyValue("--red").trim() || "#e0655f",
    ];

    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = size * dpr;
    canvas.height = size * dpr;
    ctx.scale(dpr, dpr);

    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const current: KnotParams = { ...PHASE_PARAMS.idle };
    let raf = 0;
    let lastPhase: VoiceSessionPhase | null = null;

    function draw(timeMs: number) {
      if (!ctx) return;
      const f = live.current.frame;
      const phase: VoiceSessionPhase = f?.state ?? "idle";
      const target = PHASE_PARAMS[phase];
      // Listening breathes with the actual mic level.
      const amplitude =
        phase === "listening" && f?.micLevel != null
          ? 0.08 + f.micLevel * 0.5
          : target.amplitude;
      const lerp = (a: number, b: number) => a + (b - a) * 0.06;
      current.spin = lerp(current.spin, target.spin);
      current.amplitude = lerp(current.amplitude, amplitude);
      current.glow = lerp(current.glow, target.glow);
      current.palette = lerp(current.palette, target.palette);

      const t = timeMs / 1000;
      const cx = size / 2;
      const cy = size / 2;
      const base = size * 0.27;
      ctx.clearRect(0, 0, size, size);

      const lowIdx = Math.floor(current.palette);
      const color = palette[Math.min(3, Math.max(0, Math.round(current.palette)))];
      const accent = palette[Math.min(3, lowIdx === 0 ? 0 : lowIdx - 1)];

      for (let strand = 0; strand < 3; strand++) {
        const phaseOff = (strand * Math.PI * 2) / 3;
        ctx.beginPath();
        for (let i = 0; i <= 180; i++) {
          const a = (i / 180) * Math.PI * 2;
          const wobble =
            1 +
            0.16 * Math.sin(3 * a + phaseOff + t * current.spin * 2) +
            current.amplitude * 0.5 * Math.sin(7 * a - t * current.spin * 4 + phaseOff);
          const r = base * wobble;
          const x = cx + r * Math.cos(a);
          const y = cy + r * Math.sin(a) * 0.92;
          if (i === 0) ctx.moveTo(x, y);
          else ctx.lineTo(x, y);
        }
        ctx.closePath();
        const scale = Math.max(size / 420, 0.45);
        ctx.strokeStyle = color;
        ctx.globalAlpha = 0.35 + current.glow * 0.4;
        ctx.lineWidth = 2.2 * scale;
        ctx.shadowColor = color;
        ctx.shadowBlur = (8 + current.glow * 22) * scale;
        ctx.stroke();

        // The running lights: a dashed pass in the accent color, drifting.
        ctx.setLineDash([4 * scale, 26 * scale]);
        ctx.lineDashOffset = -t * 30 * (0.4 + current.spin) - strand * 10;
        ctx.strokeStyle = accent;
        ctx.globalAlpha = 0.5 + current.glow * 0.35;
        ctx.lineWidth = 1.4 * scale;
        ctx.shadowBlur = 4 * scale;
        ctx.stroke();
        ctx.setLineDash([]);
      }
      ctx.globalAlpha = 1;
      ctx.shadowBlur = 0;
    }

    if (reduced) {
      // One frame per phase change; no loop.
      const tick = () => {
        const phase = live.current.frame?.state ?? "idle";
        if (phase !== lastPhase) {
          lastPhase = phase;
          Object.assign(current, PHASE_PARAMS[phase]);
          draw(0);
        }
        raf = requestAnimationFrame(tick);
      };
      raf = requestAnimationFrame(tick);
    } else {
      const loop = (time: number) => {
        draw(time);
        raf = requestAnimationFrame(loop);
      };
      raf = requestAnimationFrame(loop);
    }
    return () => cancelAnimationFrame(raf);
  }, [size]);

  return (
    <canvas ref={canvasRef} className="vs-knot" style={{ width: size, height: size }} />
  );
}

/**
 * The full-screen Voice Parlay experience. Pure state display: every phase
 * comes from the server's voice.state broadcast, and the controls are four
 * verbs. Text stays safe — the conversation lands in the thread either way.
 */
export function VoiceSession() {
  const { state, actions, dispatch } = useStore();
  const frame = state.voice.frame;
  const minimized = state.voice.minimized;
  const stageRef = useRef<HTMLDivElement>(null);

  const sessionId = frame?.sessionId;
  const phase: VoiceSessionPhase = frame?.state ?? "idle";
  const muted = frame?.muted ?? false;
  const canInterrupt =
    phase === "thinking" || phase === "synthesizing" || phase === "speaking";

  function end() {
    if (sessionId) void actions.stopVoiceSession(sessionId).catch(() => undefined);
    dispatch({ type: "voiceOpen", open: false });
  }

  useEffect(() => {
    if (!minimized) stageRef.current?.focus();
  }, [minimized]);

  // Docked to a corner: the session keeps running server-side and this stays
  // its live viewport while the rest of the app is used — another thread,
  // another machine, whatever. Click the knot to bring the stage back.
  if (minimized) {
    return createPortal(
      <div className={`vs-mini vs-${phase}`} role="complementary" aria-label="Voice conversation (docked)">
        <button
          type="button"
          className="vs-mini-knot"
          title="Open the voice conversation"
          onClick={() => dispatch({ type: "voiceMinimized", minimized: false })}
        >
          <VoiceKnotCanvas frame={frame} size={72} />
        </button>
        <div className="vs-mini-body">
          <span className="vs-mini-phase">{PHASE_WORDS[phase]}</span>
          <div className="vs-mini-actions">
            <button
              type="button"
              className={`vs-mini-btn ${muted ? "on" : ""}`}
              disabled={!sessionId}
              title={muted ? "Unmute" : "Mute"}
              onClick={() =>
                sessionId &&
                void actions.setVoiceMuted(sessionId, !muted).catch(() => undefined)
              }
            >
              {muted ? "unmute" : "mute"}
            </button>
            {canInterrupt && sessionId && (
              <button
                type="button"
                className="vs-mini-btn"
                title="Interrupt the reply"
                onClick={() =>
                  void actions.interruptVoice(sessionId).catch(() => undefined)
                }
              >
                stop
              </button>
            )}
            <button type="button" className="vs-mini-btn vs-end" title="End the conversation" onClick={end}>
              end
            </button>
          </div>
        </div>
      </div>,
      document.body,
    );
  }

  return createPortal(
    <div className="vs-backdrop">
      <div
        ref={stageRef}
        className={`vs-stage vs-${phase}`}
        role="dialog"
        aria-modal="true"
        aria-label="Voice conversation"
        tabIndex={-1}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            // Non-destructive by default: Esc tucks the conversation into
            // the corner; ending is the explicit button.
            e.preventDefault();
            if (sessionId) dispatch({ type: "voiceMinimized", minimized: true });
            else end();
          } else if ((e.key === "m" || e.key === "M") && sessionId) {
            e.preventDefault();
            void actions.setVoiceMuted(sessionId, !muted).catch(() => undefined);
          } else if (e.key === " " && sessionId && canInterrupt) {
            e.preventDefault();
            void actions.interruptVoice(sessionId).catch(() => undefined);
          }
        }}
      >
        <VoiceKnotCanvas frame={frame} />

        <div className="vs-phase">{PHASE_WORDS[phase]}</div>
        {frame?.detail && <div className="vs-detail">{frame.detail}</div>}
        {frame?.lastUtterance && phase !== "error" && (
          <div className="vs-transcript">“{frame.lastUtterance}”</div>
        )}

        {phase === "error" && frame?.error && (
          <div className="vs-error">
            <div className="vs-error-message">{frame.error.message}</div>
            <div className="vs-error-actions">
              {frame.threadId && (
                <button
                  type="button"
                  className="settings-toggle primary"
                  onClick={() => {
                    void actions
                      .startVoiceSession(frame.threadId!)
                      .catch(() => undefined);
                  }}
                >
                  try again
                </button>
              )}
              <button type="button" className="settings-toggle" onClick={end}>
                continue in text
              </button>
            </div>
          </div>
        )}

        <div className="vs-controls">
          <button
            type="button"
            className="vs-btn"
            disabled={!sessionId}
            title="Keep talking while you work — dock to the corner (Esc)"
            onClick={() => dispatch({ type: "voiceMinimized", minimized: true })}
          >
            minimize
          </button>
          <button
            type="button"
            className={`vs-btn ${muted ? "on" : ""}`}
            disabled={!sessionId}
            title="Mute the microphone (M)"
            onClick={() =>
              sessionId &&
              void actions.setVoiceMuted(sessionId, !muted).catch(() => undefined)
            }
          >
            {muted ? "unmute" : "mute"}
          </button>
          <button
            type="button"
            className="vs-btn"
            disabled={!sessionId || !canInterrupt}
            title="Interrupt the reply (Space)"
            onClick={() =>
              sessionId && void actions.interruptVoice(sessionId).catch(() => undefined)
            }
          >
            interrupt
          </button>
          <button type="button" className="vs-btn vs-end" title="End the conversation" onClick={end}>
            end conversation
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
