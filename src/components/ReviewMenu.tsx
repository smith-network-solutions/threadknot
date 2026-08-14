import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { Access, Agent, ReviewerPersona, Thread } from "../lib/protocol";
import { threadParticipants } from "../lib/protocol";
import { isAgentVisible } from "../lib/agentVisibility";
import { effortForModel, useFeedStore } from "../state/store";
import { effortLabel } from "./Composer";
import { AgentMark, PencilIcon, ShieldIcon, XIcon } from "./icons";

/**
 * "Review with…" — throws one or more reviewer PERSONAS at this thread
 * (Parley; see docs/PARLEY.md). A persona is a named, reusable preset: the
 * agent that powers it plus a personality folded into every brief. Built-ins
 * are seeded server-side so a first debate is one click; the pencil edits any
 * persona (or creates a new one) for those who want the weeds.
 *
 * With a single reviewer and one round it's the one-shot critique; with more
 * reviewers or more rounds it's a structured debate until everyone concedes.
 * Any agent can review — including the one that did the work: the reviewer
 * gets its own provider session and a genuinely fresh read of the transcript.
 */

const ACCESS: { id: Access; label: string }[] = [
  { id: "read", label: "Read-only" },
  { id: "edits", label: "Edits allowed" },
  { id: "full", label: "Full access" },
];

/** Per-run model/effort override for one picked persona (not persisted). */
interface PickOverride {
  model?: string;
  effort?: string;
}

/** Last debate setup, so re-running one doesn't mean rebuilding it. */
const PARLEY_SETTINGS_KEY = "threadknot.parleySettings.v2";

interface StoredParleySettings {
  personaIds: string[];
  access: Access;
  rounds: number;
  execute: boolean;
}

function loadStored(): StoredParleySettings | null {
  try {
    const raw = localStorage.getItem(PARLEY_SETTINGS_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as StoredParleySettings;
    return Array.isArray(parsed.personaIds) ? parsed : null;
  } catch {
    return null;
  }
}

function storeSettings(s: StoredParleySettings): void {
  try {
    localStorage.setItem(PARLEY_SETTINGS_KEY, JSON.stringify(s));
  } catch {
    // Preferences are a convenience; locked-down storage must not block chat.
  }
}

/**
 * Why Review can't run yet, or null when it can. Shared by the desktop header
 * pill and the phone More menu so the two never disagree about whether a
 * review is possible, or about how to explain that it isn't.
 */
export function useReviewBlock(thread: Thread | null): string | null {
  const { state } = useFeedStore();
  if (!thread) return "Nothing to review yet";
  if (thread.parley) return "A debate is already running";
  if (thread.status !== "idle") return "Wait for the current turn to finish";
  // Nothing to argue with until the thread has some history.
  if (state.feedThreadId === thread.id && state.feed.length === 0) {
    return "Nothing to review yet";
  }
  return null;
}

export function ReviewMenu({ thread }: { thread: Thread }) {
  const [open, setOpen] = useState(false);
  const blocked = useReviewBlock(thread);

  return (
    <>
      {/* Desktop only — phones reach Review through the header's More menu. */}
      <button
        type="button"
        className="head-pill review-pill"
        disabled={!!blocked}
        aria-haspopup="dialog"
        aria-label="Review with another agent"
        title={blocked ?? "Review with another agent"}
        onClick={() => setOpen(true)}
      >
        <ShieldIcon size={15} />
        <span>Review</span>
      </button>
      {open && <ReviewDialog thread={thread} onClose={() => setOpen(false)} />}
    </>
  );
}

/** Old servers carry no personas: synthesize one plain reviewer per available
 *  agent so the dialog still works (agent mark + default model, no persona). */
function fallbackPersonas(agents: { id: Agent; name: string; available: boolean }[]): ReviewerPersona[] {
  return agents
    .filter((a) => a.available)
    .map((a) => ({
      id: `fallback-${a.id}`,
      name: a.name,
      agent: a.id,
      personality: "",
      builtin: false,
      createdAt: "",
    }));
}

export function ReviewDialog({ thread, onClose }: { thread: Thread; onClose: () => void }) {
  const { state, actions } = useFeedStore();
  const agents = (state.hello?.agents ?? []).filter((a) => isAgentVisible(a.id));
  const lanes = threadParticipants(thread);
  const builder = lanes.find((p) => p.role === "builder") ?? lanes[0];
  const personas = useMemo(() => {
    const fromServer = (state.hello?.personas ?? []).filter((p) =>
      isAgentVisible(p.agent),
    );
    return fromServer.length > 0 ? fromServer : fallbackPersonas(agents);
  }, [state.hello?.personas, agents]);

  const agentAvailable = (a: Agent) => agents.find((x) => x.id === a)?.available ?? false;

  // Default panel: every persona whose agent is online — one click to debate.
  const stored = useMemo(loadStored, []);
  const [selected, setSelected] = useState<Set<string>>(() => {
    const valid = new Set(personas.map((p) => p.id));
    const fromStore = stored?.personaIds.filter((id) => valid.has(id)) ?? [];
    if (fromStore.length > 0) return new Set(fromStore);
    return new Set(personas.filter((p) => agentAvailable(p.agent)).map((p) => p.id));
  });
  const [overrides, setOverrides] = useState<Record<string, PickOverride>>({});
  const [access, setAccess] = useState<Access>(() => stored?.access ?? "full");
  const [rounds, setRounds] = useState<number>(() => stored?.rounds ?? 2);
  const [execute, setExecute] = useState<boolean>(() => stored?.execute ?? true);
  const [focus, setFocus] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [launching, setLaunching] = useState(false);
  const [editing, setEditing] = useState<ReviewerPersona | "new" | null>(null);

  const picked = personas.filter((p) => selected.has(p.id));
  const accessApplies = !picked.every((p) => p.agent === "hermes");
  const modelFor = (p: ReviewerPersona) =>
    overrides[p.id]?.model ??
    p.model ??
    agents.find((a) => a.id === p.agent)?.defaultModel ??
    agents.find((a) => a.id === p.agent)?.models[0]?.id ??
    "";
  const missingModel = picked.some((p) => !modelFor(p));
  const isDebate = picked.length > 1 || rounds > 1 || execute;

  function toggle(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function submit() {
    setLaunching(true);
    setError(null);
    storeSettings({ personaIds: [...selected], access, rounds, execute });
    try {
      await actions.startParley(
        picked.map((p) => ({
          agent: p.agent,
          model: modelFor(p),
          effort: overrides[p.id]?.effort ?? p.effort,
          access: accessApplies ? access : undefined,
          name: p.name,
          // Synthesized fallback personas (old servers) carry no stable identity.
          personaId: p.id.startsWith("fallback-") ? undefined : p.id,
          personality: p.personality || undefined,
        })),
        { rounds, execute, instructions: focus.trim() || undefined },
      );
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setLaunching(false);
    }
  }

  // Escape closes, like the other modals (not while the editor is open — it
  // handles Escape itself).
  useEffect(() => {
    if (editing) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose, editing]);

  return createPortal(
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal review-modal"
        role="dialog"
        aria-label="Review with another agent"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-head">
          <span>Review with another agent</span>
          <button className="icon-btn" aria-label="Cancel" onClick={onClose}>
            <XIcon size={14} />
          </button>
        </div>

        {editing ? (
          <PersonaEditor
            persona={editing === "new" ? null : editing}
            onDone={() => setEditing(null)}
            onDelete={(id) => {
              setSelected((prev) => {
                const next = new Set(prev);
                next.delete(id);
                return next;
              });
              setEditing(null);
            }}
          />
        ) : (
          <>
            {/* Arcade-only marquee (inert everywhere else; see styles.css). */}
            <div className="review-fight-marquee" aria-hidden="true">
              select your agents <em>●</em>{" "}
              {personas.filter((p) => agentAvailable(p.agent)).length} challengers available
            </div>
            <div className="review-modal-scroll">
              <p className="review-modal-blurb">
                Pick one or more reviewers — any agent can review, including the
                one that did the work; it gets its own session and a fresh read.
                Each round they argue, {builder?.name ?? "the builder"} answers,
                until everyone concedes or the round cap is hit. You can type
                into the thread at any point — the debate pauses for you.
              </p>

              <div className="review-field">
                <span className="review-field-label">Reviewers</span>
                <div className="review-agent-list" role="listbox" aria-label="Reviewers" aria-multiselectable="true">
                  {personas.map((p) => {
                    const on = selected.has(p.id);
                    const offline = !agentAvailable(p.agent);
                    return (
                      <div key={p.id} className={`persona-row${on ? " on" : ""}`}>
                        <button
                          type="button"
                          role="option"
                          aria-selected={on}
                          className={`agent-option persona-main${on ? " on" : ""}`}
                          disabled={offline}
                          title={p.personality || undefined}
                          onClick={() => toggle(p.id)}
                        >
                          <span className="persona-mark">
                            <AgentMark agent={p.agent} size={14} />
                          </span>
                          <span className="persona-name">{p.name}</span>
                          {offline ? (
                            <em className="agent-off">offline</em>
                          ) : p.agent === builder?.agent ? (
                            <em className="agent-off agent-off-built">built this · fresh read</em>
                          ) : null}
                          {on && <span className="review-check">✓</span>}
                        </button>
                        <button
                          type="button"
                          className="icon-btn persona-edit"
                          aria-label={`Edit ${p.name}`}
                          title={`Edit ${p.name}`}
                          onClick={() => setEditing(p)}
                        >
                          <PencilIcon size={12} />
                        </button>
                      </div>
                    );
                  })}
                  <button
                    type="button"
                    className="agent-option persona-new"
                    onClick={() => setEditing("new")}
                  >
                    + New reviewer persona…
                  </button>
                </div>
              </div>

              {picked.map((p) => {
                const info = agents.find((a) => a.id === p.agent);
                const models = info?.models ?? [];
                const model = modelFor(p);
                const currentModel = models.find((m) => m.id === model);
                const effortOptions = currentModel?.efforts ?? [];
                const providerDefault = p.agent === "claude";
                const effort = overrides[p.id]?.effort ?? p.effort;
                // Which rung of the effort ladder is lit, for the arcade
                // theme's PWR readout (mirrors the select's displayed value).
                const resolvedEffort =
                  effort && effortOptions.includes(effort)
                    ? effort
                    : providerDefault
                      ? currentModel?.defaultEffort
                      : (effortForModel(currentModel) ?? effortOptions[0]);
                const litEfforts = effortOptions.indexOf(resolvedEffort ?? "") + 1;
                return (
                  <div className="review-grid review-pick-row" key={p.id}>
                    <label className="review-field">
                      <span className="review-field-label">
                        {p.name} · {p.agent === "hermes" ? "Hermes agent" : p.agent === "claudex" ? "Profile" : "Model"}
                      </span>
                      <select
                        value={model}
                        onChange={(e) => {
                          const m = models.find((x) => x.id === e.target.value);
                          setOverrides((prev) => ({
                            ...prev,
                            [p.id]: {
                              model: e.target.value,
                              effort: effortForModel(m, effort, providerDefault),
                            },
                          }));
                        }}
                      >
                        {!currentModel && model && <option value={model}>{model}</option>}
                        {models.map((m) => (
                          <option key={m.id} value={m.id}>
                            {m.name}
                          </option>
                        ))}
                      </select>
                    </label>
                    {effortOptions.length > 0 && (
                      <label className="review-field review-effort-field">
                        <span className="review-field-label">Effort</span>
                        <select
                          value={
                            effort && effortOptions.includes(effort)
                              ? effort
                              : providerDefault
                                ? ""
                                : (effortForModel(currentModel) ?? effortOptions[0])
                          }
                          onChange={(e) =>
                            setOverrides((prev) => ({
                              ...prev,
                              [p.id]: { ...prev[p.id], effort: e.target.value || undefined },
                            }))
                          }
                        >
                          {providerDefault && (
                            <option value="">
                              Default
                              {currentModel?.defaultEffort
                                ? ` (${effortLabel(currentModel.defaultEffort)})`
                                : ""}
                            </option>
                          )}
                          {effortOptions.map((id) => (
                            <option key={id} value={id}>
                              {effortLabel(id)}
                            </option>
                          ))}
                        </select>
                      </label>
                    )}
                    {effortOptions.length > 0 && (
                      <div className="stat-pips" aria-hidden="true">
                        <span className="stat-pips-label">pwr</span>
                        {effortOptions.map((id, i) => (
                          <i key={id} className={i < litEfforts ? undefined : "off"} />
                        ))}
                      </div>
                    )}
                  </div>
                );
              })}

              <div className="review-grid">
                {accessApplies && (
                  <label className="review-field">
                    <span className="review-field-label">Reviewer access</span>
                    <select value={access} onChange={(e) => setAccess(e.target.value as Access)}>
                      {ACCESS.map((x) => (
                        <option key={x.id} value={x.id}>
                          {x.label}
                        </option>
                      ))}
                    </select>
                  </label>
                )}
                <label className="review-field">
                  <span className="review-field-label">Rounds</span>
                  <select value={rounds} onChange={(e) => setRounds(Number(e.target.value))}>
                    {[1, 2, 3, 4, 5].map((n) => (
                      <option key={n} value={n}>
                        {n === 1 ? "1 · one-shot" : `${n} · debate`}
                      </option>
                    ))}
                  </select>
                </label>
              </div>

              <label className="review-execute">
                <input
                  type="checkbox"
                  checked={execute}
                  onChange={(e) => setExecute(e.target.checked)}
                />
                <span>
                  When they converge, have {builder?.name ?? "the builder"} implement the
                  agreed fixes
                </span>
              </label>

              {accessApplies && access !== "full" && (
                <p className="review-warn">
                  {access === "read" ? (
                    <>
                      Read-only reviewers can't change files — and they'll{" "}
                      <strong>ask for your approval</strong> before running commands, which
                      pauses the debate until you click.
                    </>
                  ) : (
                    <>
                      Reviewers can edit files, but they'll{" "}
                      <strong>ask for your approval</strong> before other commands, which
                      pauses the debate until you click.
                    </>
                  )}
                </p>
              )}

              <input
                className="modal-input review-focus-input"
                type="text"
                value={focus}
                placeholder="Optional: what should they focus on?"
                aria-label="Review focus"
                autoFocus
                onChange={(e) => setFocus(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void submit();
                }}
              />
            </div>

            {error && <div className="modal-error">{error}</div>}

            <div className="modal-actions">
              <button className="btn tone-deny" disabled={launching} onClick={onClose}>
                Cancel
              </button>
              <button
                className="btn tone-allow"
                disabled={picked.length === 0 || missingModel || launching}
                title={
                  picked.length === 0
                    ? "Pick at least one reviewer"
                    : missingModel
                      ? "Pick a model / profile for every reviewer"
                      : undefined
                }
                onClick={() => void submit()}
              >
                {launching
                  ? "Seating reviewers…"
                  : isDebate
                    ? `Start debate · ${picked.length} reviewer${picked.length === 1 ? "" : "s"}`
                    : "Start review"}
              </button>
            </div>
          </>
        )}
      </div>
    </div>,
    document.body,
  );
}

/** Create or edit a reviewer persona: name, the agent that powers it, its
 *  model/effort, and the personality folded into every review brief. */
function PersonaEditor({
  persona,
  onDone,
  onDelete,
}: {
  persona: ReviewerPersona | null; // null = new
  onDone: () => void;
  onDelete: (id: string) => void;
}) {
  const { state, actions } = useFeedStore();
  const agents = (state.hello?.agents ?? []).filter((a) => isAgentVisible(a.id));
  const [name, setName] = useState(persona?.name ?? "");
  const [agent, setAgent] = useState<Agent>(
    persona?.agent ?? agents.find((a) => a.available)?.id ?? "claude",
  );
  const info = agents.find((a) => a.id === agent);
  const models = info?.models ?? [];
  const [model, setModel] = useState(persona?.model ?? "");
  const [effort, setEffort] = useState(persona?.effort ?? "");
  const [personality, setPersonality] = useState(persona?.personality ?? "");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const currentModel = models.find((m) => m.id === model);
  const effortOptions = currentModel?.efforts ?? [];

  async function save() {
    setBusy(true);
    setError(null);
    try {
      await actions.savePersona({
        id: persona?.id ?? "",
        name: name.trim(),
        agent,
        ...(model ? { model } : {}),
        ...(effort ? { effort } : {}),
        ...(persona?.access ? { access: persona.access } : {}),
        personality: personality.trim(),
        builtin: persona?.builtin ?? false,
        createdAt: persona?.createdAt ?? "",
      });
      onDone();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }

  async function remove() {
    if (!persona) return;
    setBusy(true);
    try {
      await actions.deletePersona(persona.id);
      onDelete(persona.id);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  }

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onDone();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onDone]);

  return (
    <>
      <div className="review-modal-scroll">
        <div className="review-field">
          <span className="review-field-label">Name</span>
          <input
            className="modal-input persona-name-input"
            type="text"
            value={name}
            placeholder="The Skeptic"
            autoFocus
            onChange={(e) => setName(e.target.value)}
          />
        </div>

        <div className="review-grid">
          <label className="review-field">
            <span className="review-field-label">Powered by</span>
            <select
              value={agent}
              onChange={(e) => {
                setAgent(e.target.value as Agent);
                setModel("");
                setEffort("");
              }}
            >
              {agents.map((a) => (
                <option key={a.id} value={a.id} disabled={!a.available}>
                  {a.name}
                  {!a.available ? " · offline" : ""}
                </option>
              ))}
            </select>
          </label>
          <label className="review-field">
            <span className="review-field-label">Model</span>
            <select value={model} onChange={(e) => setModel(e.target.value)}>
              <option value="">Agent default</option>
              {models.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.name}
                </option>
              ))}
            </select>
          </label>
          {effortOptions.length > 0 && (
            <label className="review-field review-effort-field">
              <span className="review-field-label">Effort</span>
              <select value={effort} onChange={(e) => setEffort(e.target.value)}>
                <option value="">Default</option>
                {effortOptions.map((id) => (
                  <option key={id} value={id}>
                    {effortLabel(id)}
                  </option>
                ))}
              </select>
            </label>
          )}
        </div>

        <div className="review-field">
          <span className="review-field-label">Personality — folded into every review brief</span>
          <textarea
            className="modal-input persona-personality-input"
            value={personality}
            placeholder="You doubt everything. Assume the work is wrong until the code itself proves otherwise…"
            rows={5}
            onChange={(e) => setPersonality(e.target.value)}
          />
        </div>

        {error && <div className="modal-error">{error}</div>}
      </div>

      <div className="modal-actions persona-editor-actions">
        {persona && (
          <button className="btn tone-danger" disabled={busy} onClick={() => void remove()}>
            Delete
          </button>
        )}
        <span className="persona-editor-spacer" />
        <button className="btn tone-deny" disabled={busy} onClick={onDone}>
          Cancel
        </button>
        <button
          className="btn tone-allow"
          disabled={!name.trim() || busy}
          onClick={() => void save()}
        >
          {busy ? "Saving…" : persona ? "Save persona" : "Create persona"}
        </button>
      </div>
    </>
  );
}

/** ROUND N: the arcade theme's fight-start announcement. Fires when a parley
 *  seats its reviewers (round 0 -> 1) and again each time the debate advances
 *  a round, driven purely by ParleyState.round so it needs no wiring into the
 *  launch path. It mounts for every theme; the base stylesheet keeps it
 *  display:none and only arcade gives it a box, so elsewhere the only cost is
 *  a timer. */
export function ParleyRoundSplash({ thread }: { thread: Thread }) {
  const round = thread.parley?.round ?? 0;
  const [shown, setShown] = useState(0);
  // Keyed by thread id, because this component is never remounted per thread:
  // a round number on its own cannot tell "the debate advanced" from "the view
  // moved to a different thread that happens to be further along".
  const prev = useRef<{ id: string; round: number }>({ id: thread.id, round });
  useEffect(() => {
    const was = prev.current;
    prev.current = { id: thread.id, round };
    // A different thread is being watched now. Arriving in the middle of a
    // debate is not a round starting, and the previous thread's splash must not
    // outlive it, so drop whatever is on screen and announce nothing.
    if (was.id !== thread.id) {
      setShown(0);
      return;
    }
    // The parley is over (or was never seated). Clear the overlay here rather
    // than leaving it to a hide timer that may already have been cleaned up.
    if (round === 0) {
      setShown(0);
      return;
    }
    if (round <= was.round) return;
    setShown(round);
    // Matches the CSS timeline: 0.95s delay + 2.6s slam, then gone.
    const t = window.setTimeout(() => setShown(0), 3800);
    return () => window.clearTimeout(t);
  }, [thread.id, round]);
  if (shown === 0) return null;
  return createPortal(
    // Keyed so a round advancing mid-splash remounts and replays the slam.
    <div className="parley-splash" key={shown} aria-hidden="true">
      <div className="ps-round">parley</div>
      <div className="ps-fight">round {shown}</div>
    </div>,
    document.body,
  );
}

/** The lanes in a multi-participant thread, for the header. Renders nothing for
 *  an ordinary single-agent thread. */
export function LaneChips({ thread }: { thread: Thread }) {
  const lanes = threadParticipants(thread);
  if (lanes.length < 2) return null;
  return (
    <span className="lane-chips">
      {lanes.map((p) => (
        <span
          key={p.id}
          className={`lane-chip${thread.activeSpeaker === p.id ? " on" : ""}`}
          style={{ ["--lane-color" as string]: p.color }}
          title={`${p.name} · ${p.settings.model} · ${
            p.settings.access === "read" ? "read-only" : p.settings.access
          }`}
        >
          <AgentMark agent={p.agent} size={11} />
          {p.name}
        </span>
      ))}
    </span>
  );
}
