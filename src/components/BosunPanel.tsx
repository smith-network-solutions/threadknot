import { useEffect, useMemo, useState } from "react";
import type {
  Access,
  Agent,
  Bosun,
  Cadence,
  Charter,
  Lookout,
  LookoutKind,
  LookoutType,
  RequestMap,
  Workspace,
  WorkspaceMember,
} from "../lib/protocol";
import { SMART_SEARCH_MODELS } from "../lib/protocol";
import { isAgentVisible } from "../lib/agentVisibility";
import {
  CHARTER_ACCESS,
  LOOKOUT_TYPES,
  MIN_INTERVAL_SECS,
  defaultBosunDraft,
  draftFromBosun,
  envToText,
  newCharter,
  newLookout,
  normalizeDraft,
  randomSecret,
  textToEnv,
  validateBosunDraft,
  type BosunDraft,
} from "../lib/bosun";
import { cadenceLabel, DAY_CHIP } from "../lib/schedule";
import { copyText, timeAgo } from "../lib/format";
import { pickAvatarImage } from "../lib/sidebarImage";
import { effortForModel, findThread, useStore } from "../state/store";
import { DangerButton } from "./Sidebar";
import { AgentSelect, effortLabel } from "./Composer";
import { WakeLine } from "./BosunWake";
import { CopyIcon, PencilIcon, PlayIcon, PlusIcon, TrashIcon, XIcon } from "./icons";

const WORK_ACCESS: { id: Access; label: string }[] = [
  { id: "read", label: "Read-only" },
  { id: "edits", label: "Edits allowed" },
  { id: "full", label: "Full access" },
];

/** Model ids the triage step accepts: the CLI's aliases plus the Claude ids
 *  the app already offers elsewhere (smart search). */
const TRIAGE_MODELS = [
  "haiku",
  "sonnet",
  "opus",
  ...SMART_SEARCH_MODELS.filter((m) => m.agent === "claude").map((m) => m.id),
];

const HOURLY_CHOICES = [1, 2, 3, 4, 6, 8, 12];

type TestResult = RequestMap["bosun.lookout.test"]["data"];

/** Workspaces a Bosun may route to: local list, stashed ones left out. */
function useBosunWorkspaces(): Workspace[] {
  const { state } = useStore();
  return useMemo(() => state.workspaces.filter((w) => !w.hidden), [state.workspaces]);
}

/** "threadknot · mac-mini" for a workspace root. */
function useMemberLabel() {
  const { state } = useStore();
  return (m: WorkspaceMember) => {
    const machine =
      m.machineId === state.hello?.machineId
        ? (state.hello?.friendlyName ?? "this machine")
        : (state.peers.find((p) => p.machineId === m.machineId)?.name ?? m.machineId);
    const name =
      m.name ?? m.path?.split("/").filter(Boolean).pop() ?? m.projectId;
    return `${name} · ${machine}`;
  };
}

function BosunAvatar({ bosun, size = 22 }: { bosun: { name: string; image?: string | null }; size?: number }) {
  return (
    <span className="bosun-avatar" style={{ width: size, height: size }}>
      {bosun.image ? <img src={bosun.image} alt="" /> : <span aria-hidden>⚓</span>}
    </span>
  );
}

// ---- list ----------------------------------------------------------------

function BosunRow({ bosun, onEdit }: { bosun: Bosun; onEdit: () => void }) {
  const { state, actions } = useStore();
  const [running, setRunning] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const home = state.workspaces.find((w) => w.id === bosun.homeWorkspaceId);

  async function runNow() {
    setRunning(true);
    setNote(null);
    try {
      const r = await actions.runBosun(bosun.id);
      setNote(
        r.wakeId
          ? `${r.signals} signal${r.signals === 1 ? "" : "s"} · waking`
          : r.signals > 0
            ? `${r.signals} signal${r.signals === 1 ? "" : "s"} queued`
            : "nothing new",
      );
    } catch (e) {
      setNote(e instanceof Error ? e.message : String(e));
    } finally {
      setRunning(false);
    }
  }

  return (
    <div className={`sched-row${bosun.enabled ? "" : " off"}`}>
      <div className="sched-row-main">
        <div className="sched-row-title">
          <BosunAvatar bosun={bosun} size={18} />
          <span className="sched-name">{bosun.name}</span>
          {home && <span className="sched-project">{home.name}</span>}
        </div>
        <div className="sched-row-when">
          <span>
            {bosun.lookouts.filter((l) => l.enabled).length} lookout
            {bosun.lookouts.filter((l) => l.enabled).length === 1 ? "" : "s"} ·{" "}
            {bosun.charters.length} charter{bosun.charters.length === 1 ? "" : "s"}
          </span>
          {!bosun.enabled && <span className="sched-next dim">· paused</span>}
        </div>
        {bosun.lastError && <div className="sched-error">{bosun.lastError}</div>}
        <div className="sched-last">
          {bosun.lastWakeAt ? `last wake ${timeAgo(bosun.lastWakeAt)}` : "never woken"}
          {note && <span className="sched-next">{note}</span>}
        </div>
      </div>
      <div className="sched-row-actions">
        <button
          type="button"
          className={`settings-toggle ${bosun.enabled ? "on" : ""}`}
          title={bosun.enabled ? "Pause this Bosun" : "Resume this Bosun"}
          onClick={() => void actions.updateBosun({ bosunId: bosun.id, enabled: !bosun.enabled })}
        >
          {bosun.enabled ? "on" : "off"}
        </button>
        <button
          type="button"
          className="icon-btn"
          title="Run now: check every lookout, then wake"
          disabled={running}
          onClick={() => void runNow()}
        >
          <PlayIcon size={13} />
        </button>
        <button type="button" className="icon-btn" title="Edit" onClick={onEdit}>
          <PencilIcon size={13} />
        </button>
        <DangerButton
          label="Delete Bosun"
          onConfirm={() => void actions.deleteBosun(bosun.id).catch(() => undefined)}
        />
      </div>
    </div>
  );
}

// ---- small field editors -------------------------------------------------

function CadenceFields({ value, onChange }: { value: Cadence; onChange: (c: Cadence) => void }) {
  const time = value.type === "hourly" ? "09:00" : value.time;
  const setType = (type: Cadence["type"]) => {
    switch (type) {
      case "hourly":
        return onChange({ type, everyHours: 2 });
      case "weekly":
        return onChange({ type, time, days: [1] });
      default:
        return onChange({ type, time });
    }
  };
  return (
    <div className="bosun-cadence">
      <div className="seg" role="group" aria-label="Cadence">
        {(
          [
            ["daily", "Daily"],
            ["weekdays", "Weekdays"],
            ["weekly", "Weekly"],
            ["hourly", "Hourly"],
          ] as [Cadence["type"], string][]
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            className={value.type === id ? "seg-btn on" : "seg-btn"}
            onClick={() => setType(id)}
          >
            {label}
          </button>
        ))}
      </div>
      {value.type === "weekly" && (
        <div className="sched-days" role="group" aria-label="Days of week">
          {DAY_CHIP.map((label, day) => (
            <button
              key={day}
              type="button"
              aria-label={`Day ${day}`}
              className={`sched-day${value.days.includes(day) ? " on" : ""}`}
              onClick={() =>
                onChange({
                  ...value,
                  days: value.days.includes(day)
                    ? value.days.filter((d) => d !== day)
                    : [...value.days, day],
                })
              }
            >
              {label}
            </button>
          ))}
        </div>
      )}
      {value.type === "hourly" ? (
        <select
          value={value.everyHours}
          onChange={(e) => onChange({ type: "hourly", everyHours: Number(e.target.value) })}
        >
          {HOURLY_CHOICES.map((h) => (
            <option key={h} value={h}>
              {h === 1 ? "every hour" : `every ${h} hours`}
            </option>
          ))}
        </select>
      ) : (
        <input
          type="time"
          value={value.time}
          onChange={(e) => onChange({ ...value, time: e.target.value || value.time })}
        />
      )}
      <span className="sched-hint">{cadenceLabel(value)}</span>
    </div>
  );
}

function NumberInput({
  value,
  min,
  onChange,
  label,
}: {
  value: number;
  min: number;
  onChange: (n: number) => void;
  label: string;
}) {
  return (
    <input
      type="number"
      className="bosun-num"
      aria-label={label}
      min={min}
      step={1}
      value={Number.isFinite(value) ? value : ""}
      onChange={(e) => onChange(e.target.value === "" ? Number.NaN : Math.floor(Number(e.target.value)))}
    />
  );
}

function TestResultView({ result }: { result: TestResult | { error: string } }) {
  if ("error" in result) return <div className="modal-error">{result.error}</div>;
  return (
    <div className="bosun-test">
      <div className="sched-last">
        {result.signals.length} signal{result.signals.length === 1 ? "" : "s"} in {result.ms} ms
        {result.watermark ? ` · watermark ${result.watermark}` : ""}
      </div>
      {result.signals.length > 0 && (
        <div className="bosun-test-scroll">
          <table className="bosun-test-table">
            <thead>
              <tr>
                <th>kind</th>
                <th>title</th>
                <th>hint</th>
                <th>ref</th>
              </tr>
            </thead>
            <tbody>
              {result.signals.map((s) => (
                <tr key={s.id} title={s.body.slice(0, 400)}>
                  <td>{s.kind}</td>
                  <td>{s.title}</td>
                  <td>{s.hints?.workspace ?? ""}</td>
                  <td className="bosun-test-ref">{s.refs?.url ?? s.refs?.path ?? ""}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {result.stderr && <pre className="bosun-stderr">{result.stderr}</pre>}
    </div>
  );
}

function LookoutEditor({
  lookout,
  bosunId,
  onChange,
  onRemove,
}: {
  lookout: Lookout;
  /** Absent until the Bosun has been saved once: test + webhook URL need it. */
  bosunId?: string;
  onChange: (l: Lookout) => void;
  onRemove: () => void;
}) {
  const { actions } = useStore();
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState<TestResult | { error: string } | null>(null);
  const [url, setUrl] = useState<string | null>(null);
  const [urlError, setUrlError] = useState<string | null>(null);
  // Env is edited as text; parsing on every keystroke would eat a half-typed line.
  const [envText, setEnvText] = useState(() =>
    lookout.kind.type === "command" ? envToText(lookout.kind.env) : "",
  );
  const kind = lookout.kind;
  const setKind = (k: LookoutKind) => onChange({ ...lookout, kind: k });
  const polls = kind.type === "command" || kind.type === "folder";

  useEffect(() => {
    if (kind.type !== "webhook" || !bosunId) return;
    let cancelled = false;
    actions
      .bosunWebhookUrl(bosunId, lookout.id)
      .then((u) => !cancelled && (setUrl(u), setUrlError(null)))
      .catch((e: unknown) => !cancelled && setUrlError(e instanceof Error ? e.message : String(e)));
    return () => {
      cancelled = true;
    };
  }, [actions, bosunId, kind.type, lookout.id]);

  async function test() {
    if (!bosunId) return;
    setTesting(true);
    setResult(null);
    try {
      setResult(await actions.testLookout(bosunId, lookout));
    } catch (e) {
      setResult({ error: e instanceof Error ? e.message : String(e) });
    } finally {
      setTesting(false);
    }
  }

  return (
    <div className={`bosun-card${lookout.enabled ? "" : " off"}`}>
      <div className="bosun-card-head">
        <span className="sched-dispatch-chip">{kind.type}</span>
        <input
          type="text"
          className="bosun-card-name"
          aria-label="Lookout name"
          value={lookout.name}
          onChange={(e) => onChange({ ...lookout, name: e.target.value })}
        />
        <button
          type="button"
          className={`settings-toggle ${lookout.enabled ? "on" : ""}`}
          title={lookout.enabled ? "Disable this lookout" : "Enable this lookout"}
          onClick={() => onChange({ ...lookout, enabled: !lookout.enabled })}
        >
          {lookout.enabled ? "on" : "off"}
        </button>
        {kind.type !== "webhook" && (
          <button
            type="button"
            className="btn bosun-small-btn"
            disabled={!bosunId || testing}
            title={bosunId ? "Run once; nothing is queued" : "Save the Bosun first"}
            onClick={() => void test()}
          >
            {testing ? "Testing…" : "Test"}
          </button>
        )}
        <button type="button" className="icon-btn" aria-label="Remove lookout" onClick={onRemove}>
          <TrashIcon size={13} />
        </button>
      </div>

      {kind.type === "command" && (
        <>
          <label className="sched-field">
            <span className="sched-label">Command</span>
            <input
              type="text"
              value={kind.command}
              placeholder="e.g. /path/to/lookouts/orbit-tasks.py"
              onChange={(e) => setKind({ ...kind, command: e.target.value })}
            />
          </label>
          <label className="sched-field">
            <span className="sched-label">
              Arguments <em className="sched-optional">one per line</em>
            </span>
            <textarea
              rows={2}
              value={kind.args.join("\n")}
              onChange={(e) => setKind({ ...kind, args: e.target.value.split("\n") })}
            />
          </label>
          <label className="sched-field">
            <span className="sched-label">
              Environment <em className="sched-optional">KEY=value, one per line</em>
            </span>
            <textarea
              rows={2}
              value={envText}
              onChange={(e) => {
                setEnvText(e.target.value);
                setKind({ ...kind, env: textToEnv(e.target.value) });
              }}
            />
          </label>
          <label className="sched-field">
            <span className="sched-label">
              Working directory <em className="sched-optional">optional, defaults to the data dir</em>
            </span>
            <input
              type="text"
              value={kind.cwd ?? ""}
              onChange={(e) => setKind({ ...kind, cwd: e.target.value || null })}
            />
          </label>
        </>
      )}

      {kind.type === "folder" && (
        <>
          <label className="sched-field">
            <span className="sched-label">Folder</span>
            <input
              type="text"
              value={kind.path}
              placeholder="e.g. ~/Calls"
              onChange={(e) => setKind({ ...kind, path: e.target.value })}
            />
          </label>
          <div className="bosun-inline">
            <label className="sched-field">
              <span className="sched-label">File name pattern</span>
              <input
                type="text"
                value={kind.pattern}
                placeholder="triage.md"
                onChange={(e) => setKind({ ...kind, pattern: e.target.value })}
              />
            </label>
            <label className="sched-field">
              <span className="sched-label">Max age (days)</span>
              <NumberInput
                label="Max age in days"
                value={kind.maxAgeDays}
                min={1}
                onChange={(n) => setKind({ ...kind, maxAgeDays: n })}
              />
            </label>
          </div>
        </>
      )}

      {kind.type === "webhook" && (
        <div className="sched-field">
          <span className="sched-label">Hail URL</span>
          {bosunId ? (
            url ? (
              <div className="bosun-copy-row">
                <code className="bosun-code">{url}</code>
                <button
                  type="button"
                  className="icon-btn"
                  aria-label="Copy URL"
                  onClick={() => void copyText(url)}
                >
                  <CopyIcon size={13} />
                </button>
              </div>
            ) : (
              <div className="sched-hint">{urlError ? `Save this lookout first (${urlError})` : "…"}</div>
            )
          ) : (
            <div className="sched-hint">Save the Bosun to get this lookout's URL.</div>
          )}
          <span className="sched-label">Bearer secret</span>
          <div className="bosun-copy-row">
            <code className="bosun-code">{kind.secret || "(none)"}</code>
            <button
              type="button"
              className="icon-btn"
              aria-label="Copy secret"
              disabled={!kind.secret}
              onClick={() => void copyText(kind.secret)}
            >
              <CopyIcon size={13} />
            </button>
            <button
              type="button"
              className="btn bosun-small-btn"
              onClick={() => setKind({ ...kind, secret: randomSecret() })}
            >
              Regenerate secret
            </button>
          </div>
          <div className="sched-hint">
            POST a JSON object with <code>Authorization: Bearer &lt;secret&gt;</code>. A new
            secret takes effect when you save.
          </div>
        </div>
      )}

      {kind.type === "timer" && (
        <>
          <div className="sched-field">
            <span className="sched-label">When</span>
            <CadenceFields value={kind.cadence} onChange={(cadence) => setKind({ ...kind, cadence })} />
          </div>
          <label className="sched-field">
            <span className="sched-label">Signal body</span>
            <textarea
              rows={2}
              value={kind.prompt}
              placeholder="e.g. Morning sweep: anything overdue across my workspaces?"
              onChange={(e) => setKind({ ...kind, prompt: e.target.value })}
            />
          </label>
        </>
      )}

      {polls && (
        <label className="sched-field">
          <span className="sched-label">
            Check every (seconds) <em className="sched-optional">min {MIN_INTERVAL_SECS}</em>
          </span>
          <NumberInput
            label="Poll interval in seconds"
            value={lookout.intervalSecs}
            min={MIN_INTERVAL_SECS}
            onChange={(n) => onChange({ ...lookout, intervalSecs: n })}
          />
        </label>
      )}

      {(lookout.lastRunAt || lookout.lastError) && (
        <div className="sched-last">
          {lookout.lastRunAt && `last ran ${timeAgo(lookout.lastRunAt)} · ${lookout.lastSignalCount} signals`}
        </div>
      )}
      {lookout.lastError && <div className="sched-error">{lookout.lastError}</div>}
      {result && <TestResultView result={result} />}
    </div>
  );
}

function CharterEditor({
  charter,
  workspace,
  onChange,
  onRemove,
}: {
  charter: Charter;
  workspace: Workspace | undefined;
  onChange: (c: Charter) => void;
  onRemove: () => void;
}) {
  const memberLabel = useMemberLabel();
  const members = workspace?.members ?? [];
  const memberIndex = charter.member
    ? members.findIndex(
        (m) => m.machineId === charter.member!.machineId && m.projectId === charter.member!.projectId,
      )
    : -1;
  return (
    <div className="bosun-card">
      <div className="bosun-card-head">
        <span className="bosun-card-title">{workspace?.name ?? charter.workspaceId}</span>
        <button type="button" className="icon-btn" aria-label="Remove charter" onClick={onRemove}>
          <TrashIcon size={13} />
        </button>
      </div>
      <label className="sched-field">
        <span className="sched-label">What this workspace is</span>
        <textarea
          rows={2}
          value={charter.summary}
          placeholder="e.g. Service Storm: field-service SaaS. Support tickets from Orbit org service-storm."
          onChange={(e) => onChange({ ...charter, summary: e.target.value })}
        />
      </label>
      <label className="sched-field">
        <span className="sched-label">
          Route hints <em className="sched-optional">one per line</em>
        </span>
        <textarea
          rows={2}
          value={charter.routeHints.join("\n")}
          placeholder={"orbit org service-storm\nsender @firecompany.com\ncaller Bill"}
          onChange={(e) => onChange({ ...charter, routeHints: e.target.value.split("\n") })}
        />
      </label>
      <label className="sched-field">
        <span className="sched-label">
          Standing orders <em className="sched-optional">markdown, prepended to every work turn</em>
        </span>
        <textarea
          rows={4}
          value={charter.standingOrders}
          placeholder="e.g. Reproduce the bug, fix it on a branch, and report. Never deploy."
          onChange={(e) => onChange({ ...charter, standingOrders: e.target.value })}
        />
      </label>
      <div className="sched-controls">
        <label className="ctl">
          <span className="ctl-label">Access</span>
          <select
            value={charter.access ?? ""}
            onChange={(e) =>
              onChange({ ...charter, access: (e.target.value || null) as Access | null })
            }
          >
            {CHARTER_ACCESS.map((a) => (
              <option key={a.id} value={a.id}>
                {a.label}
              </option>
            ))}
          </select>
        </label>
        <label className="ctl">
          <span className="ctl-label">Root</span>
          <select
            value={memberIndex >= 0 ? String(memberIndex) : charter.member ? "stale" : "auto"}
            onChange={(e) => {
              const v = e.target.value;
              if (v === "auto") onChange({ ...charter, member: null });
              else if (v !== "stale") onChange({ ...charter, member: members[Number(v)] ?? null });
            }}
          >
            <option value="auto">auto (this machine)</option>
            {charter.member && memberIndex < 0 && (
              <option value="stale">{memberLabel(charter.member)} (removed)</option>
            )}
            {members.map((m, i) => (
              <option key={`${m.machineId}:${m.projectId}`} value={String(i)}>
                {memberLabel(m)}
              </option>
            ))}
          </select>
        </label>
      </div>
      <label className="sched-check">
        <input
          type="checkbox"
          checked={charter.logThread}
          onChange={(e) => onChange({ ...charter, logThread: e.target.checked })}
        />
        <span>Keep a day log thread for this workspace</span>
      </label>
      <label className="sched-check">
        <input
          type="checkbox"
          checked={charter.allowWork}
          onChange={(e) => onChange({ ...charter, allowWork: e.target.checked })}
        />
        <span>
          Allow work here <em className="sched-optional">off: it may only log or ask</em>
        </span>
      </label>
    </div>
  );
}

// ---- form ----------------------------------------------------------------

function BosunForm({
  initial,
  onDone,
  onCreated,
}: {
  initial: BosunDraft;
  onDone: () => void;
  onCreated: (b: Bosun) => void;
}) {
  const { state, actions } = useStore();
  const [draft, setDraft] = useState(initial);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [addKind, setAddKind] = useState<LookoutType>("command");
  const [addCharter, setAddCharter] = useState("");
  const workspaces = useBosunWorkspaces();
  const patch = (p: Partial<BosunDraft>) => setDraft((d) => ({ ...d, ...p }));

  const agents = (state.hello?.agents ?? []).filter((a) => isAgentVisible(a.id));
  const agentInfo = agents.find((a) => a.id === draft.work.agent);
  const models = agentInfo?.models ?? [];
  const currentModel = models.find((m) => m.id === draft.work.settings.model);
  const effortOptions = currentModel?.efforts ?? [];
  const usesProviderEffortDefault = draft.work.agent === "claude";
  const setWork = (settings: Partial<BosunDraft["work"]["settings"]>, agent?: Agent) =>
    setDraft((d) => ({
      ...d,
      work: { agent: agent ?? d.work.agent, settings: { ...d.work.settings, ...settings } },
    }));

  function setAgent(agent: Agent) {
    const info = agents.find((a) => a.id === agent);
    const modelId = info?.defaultModel ?? info?.models[0]?.id ?? "";
    setWork(
      {
        model: modelId,
        effort: effortForModel(
          info?.models.find((m) => m.id === modelId),
          undefined,
          agent === "claude",
        ),
      },
      agent,
    );
  }

  const issues = validateBosunDraft(draft, workspaces);
  const charterIds = new Set(draft.charters.map((c) => c.workspaceId));
  const chartable = workspaces.filter((w) => !charterIds.has(w.id));

  async function save() {
    if (issues.length > 0) return;
    setSaving(true);
    setError(null);
    const d = normalizeDraft(draft);
    const body = {
      name: d.name,
      homeWorkspaceId: d.homeWorkspaceId,
      image: d.image,
      enabled: d.enabled,
      triage: d.triage,
      work: d.work,
      budget: d.budget,
      lookouts: d.lookouts,
      charters: d.charters,
    };
    try {
      if (d.id) {
        await actions.updateBosun({ bosunId: d.id, ...body, quietHours: d.quietHours });
        onDone();
      } else {
        const created = await actions.createBosun({
          ...body,
          ...(d.quietHours ? { quietHours: d.quietHours } : {}),
        });
        // Stay in the form: testing a lookout and reading a webhook URL both
        // need the saved id, and that is usually the very next thing to do.
        onCreated(created);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  const ledger = draft.id
    ? state.bosunLedger.filter((w) => w.bosunId === draft.id).slice(0, 20)
    : [];

  return (
    <div className="sched-form bosun-form">
      <div className="bosun-identity">
        <button
          type="button"
          className="bosun-avatar-btn"
          title={draft.image ? "Change avatar" : "Add an avatar"}
          onClick={() =>
            void pickAvatarImage()
              .then((img) => img && patch({ image: img }))
              .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
          }
        >
          <BosunAvatar bosun={draft} size={44} />
        </button>
        <label className="sched-field bosun-grow">
          <span className="sched-label">Name</span>
          <input
            type="text"
            autoFocus={!draft.id}
            value={draft.name}
            placeholder="e.g. Blackbeard"
            onChange={(e) => patch({ name: e.target.value })}
          />
        </label>
        <button
          type="button"
          className={`settings-toggle ${draft.enabled ? "on" : ""}`}
          title={draft.enabled ? "Enabled" : "Paused"}
          onClick={() => patch({ enabled: !draft.enabled })}
        >
          {draft.enabled ? "on" : "off"}
        </button>
      </div>
      {draft.image && (
        <button type="button" className="sched-view bosun-left" onClick={() => patch({ image: null })}>
          remove avatar
        </button>
      )}

      <label className="sched-field">
        <span className="sched-label">
          Home workspace <em className="sched-optional">where signals no charter claims go, as a question</em>
        </span>
        <select value={draft.homeWorkspaceId} onChange={(e) => patch({ homeWorkspaceId: e.target.value })}>
          <option value="" disabled>
            Choose…
          </option>
          {workspaces.map((w) => (
            <option key={w.id} value={w.id}>
              {w.name}
            </option>
          ))}
        </select>
      </label>

      <label className="sched-field">
        <span className="sched-label">
          Triage model <em className="sched-optional">one cheap call per wake</em>
        </span>
        <input
          type="text"
          list="bosun-triage-models"
          value={draft.triage.model}
          onChange={(e) => patch({ triage: { ...draft.triage, model: e.target.value } })}
        />
        <datalist id="bosun-triage-models">
          {TRIAGE_MODELS.map((m) => (
            <option key={m} value={m} />
          ))}
        </datalist>
      </label>

      <div className="sched-field">
        <span className="sched-label">Work runs with</span>
        <div className="sched-controls">
          <div className="ctl">
            <span className="ctl-label">Agent</span>
            <AgentSelect
              agents={agents}
              value={draft.work.agent}
              disabled={false}
              direction="down"
              onChange={setAgent}
            />
          </div>
          <label className="ctl">
            <span className="ctl-label">Model</span>
            <select
              value={draft.work.settings.model}
              onChange={(e) => {
                const m = models.find((x) => x.id === e.target.value);
                setWork({
                  model: e.target.value,
                  effort: effortForModel(m, draft.work.settings.effort, usesProviderEffortDefault),
                });
              }}
            >
              {!currentModel && (
                <option value={draft.work.settings.model}>
                  {draft.work.settings.model || "default"}
                </option>
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
                  draft.work.settings.effort && effortOptions.includes(draft.work.settings.effort)
                    ? draft.work.settings.effort
                    : usesProviderEffortDefault
                      ? ""
                      : (effortForModel(currentModel) ?? effortOptions[0])
                }
                onChange={(e) => setWork({ effort: e.target.value || undefined })}
              >
                {usesProviderEffortDefault && (
                  <option value="">
                    Default
                    {currentModel?.defaultEffort ? ` (${effortLabel(currentModel.defaultEffort)})` : ""}
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
              value={draft.work.settings.access}
              onChange={(e) => setWork({ access: e.target.value as Access })}
            >
              {WORK_ACCESS.map((x) => (
                <option key={x.id} value={x.id}>
                  {x.label}
                </option>
              ))}
            </select>
          </label>
        </div>
        <div className="sched-hint">
          A charter can lower access for its workspace, never raise it.
        </div>
      </div>

      <div className="bosun-inline">
        <div className="sched-field">
          <span className="sched-label">Quiet hours</span>
          {draft.quietHours ? (
            <div className="bosun-copy-row">
              <input
                type="time"
                aria-label="Quiet hours start"
                value={draft.quietHours.start}
                onChange={(e) => patch({ quietHours: { ...draft.quietHours!, start: e.target.value } })}
              />
              <span className="sched-hint">to</span>
              <input
                type="time"
                aria-label="Quiet hours end"
                value={draft.quietHours.end}
                onChange={(e) => patch({ quietHours: { ...draft.quietHours!, end: e.target.value } })}
              />
              <button type="button" className="sched-view" onClick={() => patch({ quietHours: null })}>
                clear
              </button>
            </div>
          ) : (
            <button
              type="button"
              className="btn bosun-small-btn bosun-left"
              onClick={() => patch({ quietHours: { start: "22:00", end: "07:00" } })}
            >
              Set quiet hours
            </button>
          )}
        </div>
        <div className="sched-field">
          <span className="sched-label">Budget</span>
          <div className="bosun-copy-row">
            <NumberInput
              label="Turns per hour"
              value={draft.budget.maxTurnsPerHour}
              min={1}
              onChange={(n) => patch({ budget: { ...draft.budget, maxTurnsPerHour: n } })}
            />
            <span className="sched-hint">turns / hour</span>
            <NumberInput
              label="Concurrent threads"
              value={draft.budget.maxConcurrent}
              min={1}
              onChange={(n) => patch({ budget: { ...draft.budget, maxConcurrent: n } })}
            />
            <span className="sched-hint">at once</span>
          </div>
        </div>
      </div>

      <div className="bosun-section">
        <div className="bosun-section-head">
          <span className="sched-label">Lookouts</span>
          <select value={addKind} onChange={(e) => setAddKind(e.target.value as LookoutType)}>
            {LOOKOUT_TYPES.map((t) => (
              <option key={t.id} value={t.id}>
                {t.label}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="btn bosun-small-btn"
            onClick={() => patch({ lookouts: [...draft.lookouts, newLookout(addKind)] })}
          >
            <PlusIcon size={12} /> Add
          </button>
        </div>
        {draft.lookouts.length === 0 && (
          <div className="sched-hint">
            Lookouts watch for signals: a script's output, new files in a folder, a webhook, a timer.
          </div>
        )}
        {draft.lookouts.map((l, i) => (
          <LookoutEditor
            key={l.id}
            lookout={l}
            bosunId={draft.id}
            onChange={(next) =>
              patch({ lookouts: draft.lookouts.map((x, j) => (j === i ? next : x)) })
            }
            onRemove={() => patch({ lookouts: draft.lookouts.filter((_, j) => j !== i) })}
          />
        ))}
      </div>

      <div className="bosun-section">
        <div className="bosun-section-head">
          <span className="sched-label">Charters</span>
          <select value={addCharter} onChange={(e) => setAddCharter(e.target.value)}>
            <option value="">Add a workspace…</option>
            {chartable.map((w) => (
              <option key={w.id} value={w.id}>
                {w.name}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="btn bosun-small-btn"
            disabled={!addCharter}
            onClick={() => {
              patch({ charters: [...draft.charters, newCharter(addCharter)] });
              setAddCharter("");
            }}
          >
            <PlusIcon size={12} /> Add
          </button>
        </div>
        {draft.charters.length === 0 && (
          <div className="sched-hint">
            A charter tells the Bosun what a workspace is, how to recognise its signals, and what it may do
            there. Without one, everything goes to the home workspace as a question.
          </div>
        )}
        {draft.charters.map((c, i) => (
          <CharterEditor
            key={c.workspaceId}
            charter={c}
            workspace={state.workspaces.find((w) => w.id === c.workspaceId)}
            onChange={(next) =>
              patch({ charters: draft.charters.map((x, j) => (j === i ? next : x)) })
            }
            onRemove={() => patch({ charters: draft.charters.filter((_, j) => j !== i) })}
          />
        ))}
      </div>

      {issues.length > 0 && (
        <ul className="bosun-issues">
          {issues.map((i) => (
            <li key={i.field}>{i.message}</li>
          ))}
        </ul>
      )}
      {error && <div className="modal-error">{error}</div>}

      <div className="modal-actions">
        <button type="button" className="btn tone-deny" onClick={onDone}>
          {draft.id ? "Close" : "Cancel"}
        </button>
        <button
          type="button"
          className="btn tone-allow"
          disabled={issues.length > 0 || saving}
          onClick={() => void save()}
        >
          {draft.id ? "Save changes" : "Create Bosun"}
        </button>
      </div>

      {draft.id && (
        <div className="bosun-section">
          <span className="sched-label">Recent wakes</span>
          {ledger.length === 0 ? (
            <div className="sched-hint">No wakes yet. A lookout that finds nothing costs nothing.</div>
          ) : (
            ledger.map((w) => <WakeLine key={w.id} wake={w} />)
          )}
        </div>
      )}
    </div>
  );
}

// ---- panel ---------------------------------------------------------------

export function BosunPanel({
  onClose,
  startNew = false,
}: {
  onClose: () => void;
  /** Open straight onto the create form (the sidebar's empty state). */
  startNew?: boolean;
}) {
  const { state, actions } = useStore();
  const workspaces = useBosunWorkspaces();
  const [form, setForm] = useState<BosunDraft | null>(null);
  // Remount the form when switching from "new" to "editing the new one".
  const [formKey, setFormKey] = useState(0);

  useEffect(() => {
    void actions.loadBosunLedger().catch(() => undefined);
  }, [actions]);

  function blankDraft(): BosunDraft {
    const agents = (state.hello?.agents ?? []).filter((a) => isAgentVisible(a.id));
    const claude = agents.find((a) => a.id === "claude");
    const preferred = claude ?? agents.find((a) => a.available) ?? agents[0];
    const model = preferred?.defaultModel ?? preferred?.models[0]?.id ?? "";
    const active = state.activeThreadId ? findThread(state, state.activeThreadId) : null;
    const activeWs = active
      ? workspaces.find((w) => w.members.some((m) => m.projectId === active.projectId))
      : undefined;
    return defaultBosunDraft({
      homeWorkspaceId: activeWs?.id ?? workspaces[0]?.id ?? "",
      workAgent: preferred?.id ?? "claude",
      workModel: model,
      workEffort: effortForModel(
        preferred?.models.find((m) => m.id === model),
        undefined,
        preferred?.id === "claude",
      ),
    });
  }

  // Lazily seeded once hello has arrived, so the defaults are real.
  const [seeded, setSeeded] = useState(false);
  useEffect(() => {
    if (!startNew || seeded) return;
    setSeeded(true);
    setForm(blankDraft());
  }, [startNew, seeded]);

  const names = new Map(state.bosuns.map((b) => [b.id, b.name]));
  const ledger = state.bosunLedger.slice(0, 20);

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal sched-modal bosun-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <span>{form ? (form.id ? `Edit ${form.name || "Bosun"}` : "New Bosun") : "Bosun"}</span>
          <button className="icon-btn" aria-label="Close" onClick={onClose}>
            <XIcon size={14} />
          </button>
        </div>

        {form ? (
          <BosunForm
            key={formKey}
            initial={form}
            onDone={() => setForm(null)}
            onCreated={(b) => {
              setForm(draftFromBosun(b));
              setFormKey((k) => k + 1);
            }}
          />
        ) : (
          <>
            <div className="sched-list">
              {state.bosuns.length === 0 && (
                <div className="sched-empty">
                  <span className="bosun-empty-mark" aria-hidden>
                    ⚓
                  </span>
                  <p>
                    A Bosun is an always-on agent that watches your lookouts (a ticket queue, a folder of
                    call notes, a webhook, a timer) and triages what it sees. It opens a thread only when
                    there is work to do or a question for you; everything else goes in the ledger.
                  </p>
                </div>
              )}
              {state.bosuns.map((b) => (
                <BosunRow
                  key={b.id}
                  bosun={b}
                  onEdit={() => {
                    setForm(draftFromBosun(b));
                    setFormKey((k) => k + 1);
                  }}
                />
              ))}
              {ledger.length > 0 && (
                <div className="bosun-section">
                  <span className="sched-label">Recent wakes</span>
                  {ledger.map((w) => (
                    <WakeLine
                      key={w.id}
                      wake={w}
                      bosunName={state.bosuns.length > 1 ? names.get(w.bosunId) : undefined}
                      onOpen={onClose}
                    />
                  ))}
                </div>
              )}
            </div>
            <div className="modal-actions">
              <button
                type="button"
                className="btn tone-allow sched-new-btn"
                disabled={workspaces.length === 0}
                onClick={() => {
                  setForm(blankDraft());
                  setFormKey((k) => k + 1);
                }}
              >
                <PlusIcon size={13} /> New Bosun
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
