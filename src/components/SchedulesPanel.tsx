import { useMemo, useState } from "react";
import type { Access, Agent, Cadence, Schedule, ThreadSettings } from "../lib/protocol";
import { HERMES_HOME_PROJECT_ID } from "../lib/protocol";
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
    (form.cadType !== "weekly" || form.days.length > 0);

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
        });
      } else {
        await actions.createSchedule({
          projectId: form.projectId,
          agent: form.agent,
          settings,
          name: form.name.trim() || undefined,
          prompt: form.prompt,
          cadence,
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

  return (
    <div className="sched-form">
      <label className="sched-field">
        <span className="sched-label">What should the agent do?</span>
        <textarea
          autoFocus
          rows={3}
          value={form.prompt}
          placeholder="e.g. Review yesterday's commits and write a short status summary. Flag anything that looks risky."
          onChange={(e) => patch({ prompt: e.target.value })}
        />
      </label>

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
          {/* The Hermes home isn't a folder — schedules need a real project. */}
          {state.projects
            .filter((p) => p.id !== HERMES_HOME_PROJECT_ID)
            .map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
        </select>
      </label>

      <div className="sched-field">
        <span className="sched-label">Run with</span>
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
            Runs may pause to ask for approval at this access level — you'll get a
            notification when one is waiting.
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
    const realProjects = state.projects.filter((p) => p.id !== HERMES_HOME_PROJECT_ID);
    const contextProjectId =
      (state.activeThreadId && findThread(state, state.activeThreadId)?.projectId) ||
      state.draft?.projectId;
    const activeProjectId =
      (contextProjectId !== HERMES_HOME_PROJECT_ID && contextProjectId) ||
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
                    notified when it finishes.
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
                  state.projects.filter((p) => p.id !== HERMES_HOME_PROJECT_ID)
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
