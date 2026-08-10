import { useMemo, useState } from "react";
import type {
  Access,
  Agent,
  Cadence,
  Schedule,
  ScheduleDispatch,
  ThreadSettings,
} from "../lib/protocol";
import { HERMES_HOME_PROJECT_ID, isQuickHomeProjectId } from "../lib/protocol";
import { isAgentVisible } from "../lib/agentVisibility";
import { cadenceLabel, DAY_CHIP, nextOccurrence, untilLabel } from "../lib/schedule";
import { timeAgo } from "../lib/format";
import { effortForModel, findThread, useStore } from "../state/store";
import { DangerButton } from "./Sidebar";
import { AgentSelect, effortLabel } from "./Composer";
import { AgentMark, ClockIcon, PencilIcon, PlayIcon, PlusIcon, XIcon } from "./icons";

type CadenceType = Cadence["type"];

const HOURLY_CHOICES = [1, 2, 3, 4, 6, 8, 12];

const ACCESS: { id: Access; label: string }[] = [
  { id: "read", label: "Read-only" },
  { id: "edits", label: "Edits allowed" },
  { id: "full", label: "Full access" },
];

interface FormState {
  editingId: string | null;
  prompt: string;
  name: string;
  projectId: string;
  cadType: CadenceType;
  time: string;
  days: number[];
  everyHours: number;
  agent: Agent;
  model: string;
  effort?: string;
  access: Access;
  /** Dispatch mode: each firing hands the brief to workers instead of running
   *  it in the schedule's own thread. */
  delegate: boolean;
  /** Target machine ids. Empty means this machine. */
  machines: string[];
  /** Root to work in on each target, by name or path fragment. */
  root: string;
  syncRef: boolean;
}

/** Every machine a dispatch can be sent to: this one, then the paired peers.
 *  A peer paired before the encrypted mesh is listed but not selectable —
 *  saying only "offline" would send someone hunting a network fault instead of
 *  updating Threadknot there and re-pairing. */
function useTargets() {
  const { state } = useStore();
  return useMemo(() => {
    const self = {
      id: state.hello?.machineId ?? "",
      name: state.hello?.friendlyName ?? "This machine",
      online: true,
      blocked: false as boolean,
      why: "",
    };
    const peers = state.peers.map((p) => ({
      id: p.machineId,
      name: p.name,
      online: !!p.online,
      blocked: !!p.needsUpgrade,
      why: p.needsUpgrade
        ? "paired before encrypted mesh — update Threadknot there and pair again"
        : p.online
          ? ""
          : "offline right now; a firing will be refused and reported",
    }));
    return [self, ...peers].filter((t) => t.id);
  }, [state.hello?.machineId, state.hello?.friendlyName, state.peers]);
}

function buildCadence(f: FormState): Cadence {
  switch (f.cadType) {
    case "hourly":
      return { type: "hourly", everyHours: f.everyHours };
    case "daily":
      return { type: "daily", time: f.time };
    case "weekdays":
      return { type: "weekdays", time: f.time };
    case "weekly":
      return { type: "weekly", days: f.days, time: f.time };
  }
}

/** One row in the schedules list. */
function ScheduleRow({
  schedule,
  onEdit,
  onClose,
}: {
  schedule: Schedule;
  onEdit: () => void;
  onClose: () => void;
}) {
  const { state, actions } = useStore();
  const [running, setRunning] = useState(false);
  const project = state.projects.find((p) => p.id === schedule.projectId);
  const next = schedule.nextRunAt ? new Date(schedule.nextRunAt) : null;
  const lastThread = schedule.lastThreadId ? findThread(state, schedule.lastThreadId) : null;
  const targets = useTargets();
  // Ids, because that is what the schedule stores; fall back to the raw value
  // so a machine that has since been unpaired still shows as something.
  const targetNames = (schedule.dispatch?.machines ?? []).map(
    (id) => targets.find((t) => t.id === id)?.name ?? id,
  );

  async function runNow() {
    setRunning(true);
    try {
      const threadId = await actions.runSchedule(schedule.id);
      await actions.selectThread(threadId);
      onClose();
    } catch {
      setRunning(false);
    }
  }

  return (
    <div className={`sched-row${schedule.enabled ? "" : " off"}`}>
      <div className="sched-row-main">
        <div className="sched-row-title">
          <AgentMark agent={schedule.agent} size={13} />
          <span className="sched-name">{schedule.name}</span>
          {project && <span className="sched-project">{project.name}</span>}
        </div>
        <div className="sched-row-when">
          <ClockIcon size={12} />
          <span>{cadenceLabel(schedule.cadence)}</span>
          {schedule.enabled && next && (
            <span className="sched-next">· next {untilLabel(next)}</span>
          )}
          {!schedule.enabled && <span className="sched-next dim">· paused</span>}
        </div>
        {schedule.dispatch && (
          <div className="sched-row-when">
            <span className="sched-dispatch-chip">dispatch</span>
            <span className="sched-next">
              {targetNames.length > 0 ? targetNames.join(", ") : "this machine"}
            </span>
          </div>
        )}
        {schedule.lastError && <div className="sched-error">{schedule.lastError}</div>}
        {!schedule.lastError && schedule.lastRunAt && (
          <div className="sched-last">
            last ran {timeAgo(schedule.lastRunAt)}
            {lastThread && schedule.lastThreadId && (
              <button
                type="button"
                className="sched-view"
                onClick={() => {
                  void actions.selectThread(schedule.lastThreadId!);
                  onClose();
                }}
              >
                view result
              </button>
            )}
          </div>
        )}
      </div>
      <div className="sched-row-actions">
        <button
          type="button"
          className={`settings-toggle ${schedule.enabled ? "on" : ""}`}
          title={schedule.enabled ? "Pause this schedule" : "Resume this schedule"}
          onClick={() =>
            void actions.updateSchedule({
              scheduleId: schedule.id,
              enabled: !schedule.enabled,
            })
          }
        >
          {schedule.enabled ? "on" : "off"}
        </button>
        <button
          type="button"
          className="icon-btn"
          title="Run now"
          disabled={running}
          onClick={() => void runNow()}
        >
          <PlayIcon size={13} />
        </button>
        <button type="button" className="icon-btn" title="Edit" onClick={onEdit}>
          <PencilIcon size={13} />
        </button>
        <DangerButton
          label="Delete schedule"
          onConfirm={() => void actions.deleteSchedule(schedule.id)}
        />
      </div>
    </div>
  );
}

function ScheduleForm({
  initial,
  onDone,
}: {
  initial: FormState;
  onDone: () => void;
}) {
  const { state, actions } = useStore();
  const [form, setForm] = useState(initial);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const targets = useTargets();

  const agents = state.hello?.agents ?? [];
  const agentInfo = agents.find((a) => a.id === form.agent);
  const models = agentInfo?.models ?? [];
  const currentModel = models.find((m) => m.id === form.model);
  const effortOptions = currentModel?.efforts ?? [];
  const usesProviderEffortDefault = form.agent === "claude";

  const patch = (p: Partial<FormState>) => setForm((f) => ({ ...f, ...p }));

  const cadence = buildCadence(form);
  const next = useMemo(() => nextOccurrence(cadence), [cadence]);
  const valid =
    form.prompt.trim().length > 0 &&
    form.projectId.length > 0 &&
    (form.cadType !== "weekly" || form.days.length > 0) &&
    (!form.delegate || form.machines.length > 0);

  /** The dispatch block, or `null` to switch the schedule back to running
   *  here. Never `undefined`: on update that would mean "leave the mode alone",
   *  and turning delegation OFF has to reach the server. */
  function dispatchBlock(): ScheduleDispatch | null {
    if (!form.delegate) return null;
    return {
      machines: form.machines,
      root: form.root.trim() || undefined,
      syncRef: form.syncRef,
      // agent/model/effort deliberately omitted: the worker inherits the
      // coordinator's, which is exactly what the "Each worker runs with"
      // controls above set. Naming them again here could only disagree.
    };
  }

  async function save() {
    if (!valid) return;
    setSaving(true);
    setError(null);
    const settings: ThreadSettings = {
      model: form.model,
      effort: effortForModel(currentModel, form.effort, usesProviderEffortDefault),
      access: form.access,
      mode: "build",
    };
    const dispatch = dispatchBlock();
    try {
      if (form.editingId) {
        await actions.updateSchedule({
          scheduleId: form.editingId,
          name: form.name.trim() || undefined,
          prompt: form.prompt,
          cadence,
          agent: form.agent,
          settings,
          projectId: form.projectId,
          dispatch,
        });
      } else {
        await actions.createSchedule({
          projectId: form.projectId,
          agent: form.agent,
          settings,
          name: form.name.trim() || undefined,
          prompt: form.prompt,
          cadence,
          dispatch: dispatch ?? undefined,
        });
      }
      onDone();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setSaving(false);
    }
  }

  function setAgent(agent: Agent) {
    const info = agents.find((a) => a.id === agent);
    const modelId = info?.defaultModel ?? info?.models[0]?.id ?? "";
    patch({
      agent,
      model: modelId,
      effort: effortForModel(
        info?.models.find((m) => m.id === modelId),
        undefined,
        agent === "claude",
      ),
    });
  }

  const remoteTargets = targets.filter(
    (t) => form.machines.includes(t.id) && t.id !== state.hello?.machineId,
  );

  return (
    <div className="sched-form">
      <div className="sched-field">
        <span className="sched-label">Each run…</span>
        <div className="seg" role="group" aria-label="What a firing does">
          {(
            [
              [false, "Runs here"],
              [true, "Dispatches to machines"],
            ] as [boolean, string][]
          ).map(([id, label]) => (
            <button
              key={String(id)}
              type="button"
              className={form.delegate === id ? "seg-btn on" : "seg-btn"}
              onClick={() => patch({ delegate: id })}
            >
              {label}
            </button>
          ))}
        </div>
        {form.delegate && (
          <div className="sched-hint">
            The schedule's own thread becomes the crew panel: it runs nothing
            itself, and every worker reports back into it.
          </div>
        )}
      </div>

      <label className="sched-field">
        <span className="sched-label">
          {form.delegate
            ? "What should each worker do?"
            : "What should the agent do?"}
        </span>
        <textarea
          autoFocus
          rows={3}
          value={form.prompt}
          placeholder={
            form.delegate
              ? "e.g. Pull the latest master and produce a release build. Report the version and where the binary landed."
              : "e.g. Review yesterday's commits and write a short status summary. Flag anything that looks risky."
          }
          onChange={(e) => patch({ prompt: e.target.value })}
        />
      </label>

      {form.delegate && (
        <div className="sched-field">
          <span className="sched-label">On machines</span>
          <div className="sched-days" role="group" aria-label="Target machines">
            {targets.map((t) => (
              <button
                key={t.id}
                type="button"
                disabled={t.blocked}
                title={t.why || undefined}
                className={`sched-machine${form.machines.includes(t.id) ? " on" : ""}${
                  t.online ? "" : " stale"
                }`}
                onClick={() =>
                  patch({
                    machines: form.machines.includes(t.id)
                      ? form.machines.filter((m) => m !== t.id)
                      : [...form.machines, t.id],
                  })
                }
              >
                {t.name}
                {!t.online && <span className="sched-machine-note">offline</span>}
              </button>
            ))}
          </div>
          {form.machines.length === 0 && (
            <div className="sched-hint">Pick at least one machine.</div>
          )}
          {remoteTargets.some((t) => !t.online) && (
            <div className="sched-hint">
              An offline machine isn't skipped quietly — the run reports which
              targets refused, so a missing platform can't pass for a success.
            </div>
          )}
        </div>
      )}

      {form.delegate && (
        <label className="sched-field">
          <span className="sched-label">
            Root on each machine{" "}
            <em className="sched-optional">
              optional — by name or path, blank picks that machine's one root in
              this workspace
            </em>
          </span>
          <input
            type="text"
            value={form.root}
            placeholder="e.g. threadknot"
            onChange={(e) => patch({ root: e.target.value })}
          />
        </label>
      )}

      {form.delegate && remoteTargets.length > 0 && (
        <label className="sched-check">
          <input
            type="checkbox"
            checked={form.syncRef}
            onChange={(e) => patch({ syncRef: e.target.checked })}
          />
          <span>
            Push this machine's commit to each worker first
            <em className="sched-optional">
              — and refuse the run rather than build a different one
            </em>
          </span>
        </label>
      )}

      <label className="sched-field">
        <span className="sched-label">
          Name <em className="sched-optional">optional — used as the thread title</em>
        </span>
        <input
          type="text"
          value={form.name}
          placeholder="e.g. Morning triage"
          onChange={(e) => patch({ name: e.target.value })}
        />
      </label>

      <label className="sched-field">
        <span className="sched-label">In project</span>
        <select
          value={form.projectId}
          onChange={(e) => patch({ projectId: e.target.value })}
        >
          {/* Hidden conversation homes aren't folders — schedules need a real project. */}
          {state.projects
            .filter(
              (p) => p.id !== HERMES_HOME_PROJECT_ID && !isQuickHomeProjectId(p.id),
            )
            .map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
        </select>
      </label>

      <div className="sched-field">
        <span className="sched-label">
          {form.delegate ? "Each worker runs with" : "Run with"}
        </span>
        <div className="sched-controls">
          <div className="ctl">
            <span className="ctl-label">Agent</span>
            <AgentSelect
              agents={agents}
              value={form.agent}
              disabled={false}
              direction="down"
              onChange={setAgent}
            />
          </div>
          <label className="ctl">
            <span className="ctl-label">Model</span>
            <select value={form.model} onChange={(e) => {
              const m = models.find((x) => x.id === e.target.value);
              patch({
                model: e.target.value,
                effort: effortForModel(m, form.effort, usesProviderEffortDefault),
              });
            }}>
              {!currentModel && form.model && (
                <option value={form.model}>{form.model}</option>
              )}
              {models.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.name}
                </option>
              ))}
            </select>
          </label>
          {effortOptions.length > 0 && (
            <label className="ctl">
              <span className="ctl-label">Effort</span>
              <select
                value={
                  form.effort && effortOptions.includes(form.effort)
                    ? form.effort
                    : usesProviderEffortDefault
                      ? ""
                      : (effortForModel(currentModel) ?? effortOptions[0])
                }
                onChange={(e) => patch({ effort: e.target.value || undefined })}
              >
                {usesProviderEffortDefault && (
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
          <label className="ctl">
            <span className="ctl-label">Access</span>
            <select
              value={form.access}
              onChange={(e) => patch({ access: e.target.value as Access })}
            >
              {ACCESS.map((x) => (
                <option key={x.id} value={x.id}>
                  {x.label}
                </option>
              ))}
            </select>
          </label>
        </div>
        {form.access !== "full" && (
          <div className="sched-hint">
            {form.delegate
              ? "A worker that hits this access level stops and asks — in its own thread, on its own machine. Full access is what an unattended dispatch usually wants."
              : "Runs may pause to ask for approval at this access level — you'll get a notification when one is waiting."}
          </div>
        )}
        {form.delegate && (
          <div className="sched-hint">
            Each machine can cap what it will accept (Settings → machines), and
            the lower of the two wins.
          </div>
        )}
      </div>

      <div className="sched-field">
        <span className="sched-label">How often?</span>
        <div className="seg" role="group" aria-label="Cadence">
          {(
            [
              ["daily", "Daily"],
              ["weekdays", "Weekdays"],
              ["weekly", "Weekly"],
              ["hourly", "Hourly"],
            ] as [CadenceType, string][]
          ).map(([id, label]) => (
            <button
              key={id}
              type="button"
              className={form.cadType === id ? "seg-btn on" : "seg-btn"}
              onClick={() => patch({ cadType: id })}
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      {form.cadType === "weekly" && (
        <div className="sched-field">
          <span className="sched-label">On days</span>
          <div className="sched-days" role="group" aria-label="Days of week">
            {DAY_CHIP.map((label, day) => (
              <button
                key={day}
                type="button"
                aria-label={`Day ${day}`}
                className={`sched-day${form.days.includes(day) ? " on" : ""}`}
                onClick={() =>
                  patch({
                    days: form.days.includes(day)
                      ? form.days.filter((d) => d !== day)
                      : [...form.days, day],
                  })
                }
              >
                {label}
              </button>
            ))}
          </div>
        </div>
      )}

      {form.cadType === "hourly" ? (
        <label className="sched-field">
          <span className="sched-label">Every</span>
          <select
            value={form.everyHours}
            onChange={(e) => patch({ everyHours: Number(e.target.value) })}
          >
            {HOURLY_CHOICES.map((h) => (
              <option key={h} value={h}>
                {h === 1 ? "hour" : `${h} hours`}
              </option>
            ))}
          </select>
        </label>
      ) : (
        <label className="sched-field">
          <span className="sched-label">At</span>
          <input
            type="time"
            value={form.time}
            onChange={(e) => patch({ time: e.target.value || form.time })}
          />
        </label>
      )}

      <div className="sched-preview">
        <ClockIcon size={13} />
        <span>
          {cadenceLabel(cadence)}
          {next && ` · first run ${untilLabel(next)}`}
          {!next && form.cadType === "weekly" && " · pick at least one day"}
          {form.delegate &&
            form.machines.length > 0 &&
            ` · ${form.machines.length} worker${form.machines.length === 1 ? "" : "s"}`}
        </span>
      </div>

      {error && <div className="modal-error">{error}</div>}

      <div className="modal-actions">
        <button type="button" className="btn tone-deny" onClick={onDone}>
          Cancel
        </button>
        <button
          type="button"
          className="btn tone-allow"
          disabled={!valid || saving}
          onClick={() => void save()}
        >
          {form.editingId ? "Save changes" : "Create scheduled run"}
        </button>
      </div>
    </div>
  );
}

export function SchedulesPanel({ onClose }: { onClose: () => void }) {
  const { state } = useStore();
  const [form, setForm] = useState<FormState | null>(null);
  const visibleSchedules = state.schedules.filter((s) => isAgentVisible(s.agent));

  function blankForm(): FormState {
    const agents = (state.hello?.agents ?? []).filter((a) => isAgentVisible(a.id));
    const preferred = agents.find((a) => a.available) ?? agents[0];
    const realProjects = state.projects.filter(
      (p) => p.id !== HERMES_HOME_PROJECT_ID && !isQuickHomeProjectId(p.id),
    );
    const contextProjectId =
      (state.activeThreadId && findThread(state, state.activeThreadId)?.projectId) ||
      state.draft?.projectId;
    const activeProjectId =
      (contextProjectId !== HERMES_HOME_PROJECT_ID &&
        !isQuickHomeProjectId(contextProjectId) &&
        contextProjectId) ||
      realProjects[0]?.id ||
      "";
    const model = preferred?.defaultModel ?? preferred?.models[0]?.id ?? "";
    return {
      editingId: null,
      prompt: "",
      name: "",
      projectId: activeProjectId,
      cadType: "weekdays",
      time: "09:00",
      days: [1],
      everyHours: 2,
      agent: preferred?.id ?? "claude",
      model,
      effort: effortForModel(
        preferred?.models.find((m) => m.id === model),
        undefined,
        preferred?.id === "claude",
      ),
      access: "edits",
      delegate: false,
      // Pre-seeded with this machine so switching to dispatch mode is one
      // click rather than a mode with nothing in it.
      machines: state.hello?.machineId ? [state.hello.machineId] : [],
      root: "",
      syncRef: false,
    };
  }

  function editForm(s: Schedule): FormState {
    return {
      editingId: s.id,
      prompt: s.prompt,
      name: s.name,
      projectId: s.projectId,
      cadType: s.cadence.type,
      time: s.cadence.type !== "hourly" ? s.cadence.time : "09:00",
      days: s.cadence.type === "weekly" ? s.cadence.days : [1],
      everyHours: s.cadence.type === "hourly" ? s.cadence.everyHours : 2,
      agent: s.agent,
      model: s.settings.model,
      effort: s.settings.effort,
      access: s.settings.access,
      delegate: !!s.dispatch,
      // A stored empty list means "this machine" on the server; show that.
      machines:
        s.dispatch?.machines?.length
          ? s.dispatch.machines
          : state.hello?.machineId
            ? [state.hello.machineId]
            : [],
      root: s.dispatch?.root ?? "",
      syncRef: !!s.dispatch?.syncRef,
    };
  }

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal sched-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <span>
            {form ? (form.editingId ? "Edit scheduled run" : "New scheduled run") : "Scheduled runs"}
          </span>
          <button className="icon-btn" aria-label="Close" onClick={onClose}>
            <XIcon size={14} />
          </button>
        </div>

        {form ? (
          <ScheduleForm initial={form} onDone={() => setForm(null)} />
        ) : (
          <>
            <div className="sched-list">
              {visibleSchedules.length === 0 && (
                <div className="sched-empty">
                  <ClockIcon size={22} />
                  <p>
                    Run an agent on a schedule — every morning, hourly, whenever.
                    Each run lands as a fresh thread in its project, and you'll be
                    notified when it finishes. A run can also dispatch the work to
                    your other machines, which is how one schedule produces a
                    build for all three.
                  </p>
                </div>
              )}
              {visibleSchedules.map((s) => (
                <ScheduleRow
                  key={s.id}
                  schedule={s}
                  onEdit={() => setForm(editForm(s))}
                  onClose={onClose}
                />
              ))}
            </div>
            <div className="modal-actions">
              <button
                type="button"
                className="btn tone-allow sched-new-btn"
                disabled={
                  state.projects.filter(
                    (p) =>
                      p.id !== HERMES_HOME_PROJECT_ID &&
                      !isQuickHomeProjectId(p.id),
                  )
                    .length === 0
                }
                onClick={() => setForm(blankForm())}
              >
                <PlusIcon size={13} /> New scheduled run
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
