/**
 * Settings → Library: everything installed that extends what the agents can do.
 *
 * Two shelves behind one machine picker, because they answer the same question
 * ("what has this machine been given?") through different mechanisms:
 *
 * * **Skills** are folders each CLI discovers on its own. Threadknot writes them
 *   into `~/.claude/skills`, `~/.codex/skills` and `~/.kimi-code/skills`, which
 *   is why a skill installed here also works in a plain terminal session — and
 *   why the same folder installed twice is ONE row with two badges.
 * * **MCP servers** have no shared location, so Threadknot keeps the registry and
 *   injects enabled entries into every driver at spawn.
 *
 * The viewer is deliberately honest about what it does not own: skills that
 * came from a Claude Code plugin are listed and labelled, but their Remove
 * button is disabled — the marketplace owns them, and quietly deleting a
 * plugin's folder would just have it reappear on the next update.
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { useStore } from "../state/store";
import { SKILL_TARGETS } from "../lib/protocol";
import type {
  CatalogMcp,
  InstalledSkill,
  LibraryData,
  McpServerInfo,
  McpTransport,
  SkillTarget,
} from "../lib/protocol";
import { MachineAvatar, machineLook } from "./MachineAvatar";

type Shelf = "skills" | "mcp";

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** One-line summary of how a server is reached, for the card's meta row. */
function transportSummary(transport: McpTransport): string {
  if (transport.type === "http") return transport.url;
  return [transport.command, ...transport.args].join(" ");
}

function originLabel(skill: InstalledSkill): string {
  switch (skill.origin.kind) {
    case "library":
      return skill.source ?? "installed by Threadknot";
    case "plugin":
      return `from the ${skill.origin.plugin} plugin`;
    default:
      return "added by hand";
  }
}

export function LibrarySettings() {
  const { state, actions } = useStore();
  const [shelf, setShelf] = useState<Shelf>("skills");
  /** Whose library is being managed: undefined = this machine, else a peer. */
  const [machine, setMachine] = useState<string | undefined>(undefined);
  const [data, setData] = useState<LibraryData | null>(null);
  const [loadError, setLoadError] = useState("");

  const localId = state.hello?.machineId;
  const localName = state.hello?.friendlyName ?? "this machine";
  const isLocal = machine === undefined || machine === localId;
  const machineName = isLocal
    ? localName
    : (state.peers.find((p) => p.machineId === machine)?.name ?? "that machine");

  const reload = useCallback(() => {
    setLoadError("");
    void actions
      .listLibrary(machine)
      .then(setData)
      .catch((e) => {
        setData(null);
        setLoadError(e instanceof Error ? e.message : String(e));
      });
  }, [actions, machine]);

  // Reloaded on mount, on machine switch, and after every mutation below. The
  // server also pulses `library`, but Settings is a foreground surface — the
  // shelf a user is looking at is the one they are changing.
  useEffect(reload, [reload]);

  return (
    <>
      <div className="settings-block">
        <div className="settings-label">machine</div>
        <div className="archive-machines">
          <button
            type="button"
            className={`archive-chip${isLocal ? " on" : ""}`}
            aria-current={isLocal}
            onClick={() => setMachine(undefined)}
          >
            <MachineAvatar {...machineLook(state, undefined)} size={18} preview={false} />
            <span className="archive-chip-name">{localName}</span>
            <span className="archive-chip-dot online" title="online" />
          </button>
          {state.peers.map((p) => (
            <button
              key={p.machineId}
              type="button"
              className={`archive-chip${machine === p.machineId ? " on" : ""}`}
              aria-current={machine === p.machineId}
              onClick={() => setMachine(p.machineId)}
            >
              <MachineAvatar {...machineLook(state, p.machineId)} size={18} preview={false} />
              <span className="archive-chip-name">{p.name}</span>
              <span
                className={`archive-chip-dot${p.online ? " online" : ""}`}
                title={p.online ? "online" : "offline"}
              />
            </button>
          ))}
        </div>
      </div>

      <div className="lib-shelves" role="tablist" aria-label="Library shelves">
        <button
          type="button"
          role="tab"
          aria-selected={shelf === "skills"}
          className={`lib-shelf${shelf === "skills" ? " on" : ""}`}
          onClick={() => setShelf("skills")}
        >
          Skills
          {data && <span className="lib-shelf-count">{data.skills.length}</span>}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={shelf === "mcp"}
          className={`lib-shelf${shelf === "mcp" ? " on" : ""}`}
          onClick={() => setShelf("mcp")}
        >
          MCP servers
          {data && <span className="lib-shelf-count">{data.mcpServers.length}</span>}
        </button>
      </div>

      {loadError && <div className="bl-empty">{loadError}</div>}
      {!loadError && !data && <div className="bl-empty">Reading the shelf…</div>}

      {data && shelf === "skills" && (
        <SkillsShelf
          data={data}
          machine={machine}
          machineName={isLocal ? "this machine" : machineName}
          reload={reload}
        />
      )}
      {data && shelf === "mcp" && (
        <McpShelf data={data} machine={machine} reload={reload} />
      )}
    </>
  );
}

/* -------------------------------------------------------------------------- */
/* Skills                                                                      */
/* -------------------------------------------------------------------------- */

function SkillsShelf({
  data,
  machine,
  machineName,
  reload,
}: {
  data: LibraryData;
  machine?: string;
  machineName: string;
  reload: () => void;
}) {
  const { actions } = useStore();
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [confirm, setConfirm] = useState<string | null>(null);
  const [customSource, setCustomSource] = useState("");
  /** Agents a catalog/custom install writes to. */
  const [targets, setTargets] = useState<SkillTarget[]>(["claude"]);

  const installed = useMemo(
    () => new Set(data.skills.map((s) => s.id)),
    [data.skills],
  );

  const run = async (key: string, fn: () => Promise<unknown>) => {
    setBusy(key);
    setError("");
    try {
      await fn();
      reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy("");
    }
  };

  const toggleTarget = (id: SkillTarget) =>
    setTargets((current) =>
      current.includes(id) ? current.filter((t) => t !== id) : [...current, id],
    );

  const categories = useMemo(() => {
    const seen: string[] = [];
    for (const entry of data.catalog.skills) {
      if (!seen.includes(entry.category)) seen.push(entry.category);
    }
    return seen;
  }, [data.catalog.skills]);

  return (
    <>
      <div className="settings-block">
        <div className="settings-label">how skills work</div>
        <div className="lib-note">
          A skill is a folder with a <code>SKILL.md</code> in it. Threadknot writes
          that folder into each agent’s own skills directory, so the agent finds
          it by itself — the same skill also works in a plain terminal session.
          Installing for more than one agent writes the same folder to each.
        </div>
      </div>

      <div className="settings-block">
        <div className="settings-label">installed on {machineName}</div>
        {data.skills.length === 0 && (
          <div className="bl-empty">
            No skills here yet. Pick some from the catalog below.
          </div>
        )}
        {data.skills.map((skill) => {
          const missing = SKILL_TARGETS.filter((t) => !skill.targets.includes(t.id));
          return (
            <div key={skill.id} className="lib-card">
              <div className="lib-card-head">
                <div className="lib-card-title">{skill.title}</div>
                <div className="lib-badges">
                  {SKILL_TARGETS.filter((t) => skill.targets.includes(t.id)).map((t) => (
                    <span key={t.id} className={`lib-badge ${t.id}`}>
                      {t.label}
                    </span>
                  ))}
                </div>
              </div>
              {skill.description && (
                <div className="lib-card-desc">{skill.description}</div>
              )}
              <div className="lib-card-meta">
                <span className="lib-meta-origin">{originLabel(skill)}</span>
                {skill.alsoFromPlugin && (
                  <span
                    className="lib-meta-warn"
                    title={`The ${skill.alsoFromPlugin} plugin ships a skill with this name too. Removing this copy will not stop Claude Code from finding the plugin's.`}
                  >
                    also in the {skill.alsoFromPlugin} plugin
                  </span>
                )}
                <span>
                  {skill.files} file{skill.files === 1 ? "" : "s"} ·{" "}
                  {formatBytes(skill.bytes)}
                </span>
                <span className="lib-meta-path" title={skill.path}>
                  {skill.path}
                </span>
              </div>
              <div className="lib-card-actions">
                {missing.length > 0 && (
                  <div className="lib-add-to">
                    <span className="lib-add-to-label">also install for</span>
                    {missing.map((t) => (
                      <button
                        key={t.id}
                        type="button"
                        className="lib-mini-btn"
                        disabled={busy === `copy:${skill.id}:${t.id}`}
                        onClick={() =>
                          void run(`copy:${skill.id}:${t.id}`, () =>
                            actions.copySkill(skill.id, [t.id], machine),
                          )
                        }
                      >
                        {busy === `copy:${skill.id}:${t.id}` ? "…" : t.label}
                      </button>
                    ))}
                  </div>
                )}
                {!skill.removable ? (
                  <span
                    className="lib-locked"
                    title="A Claude Code plugin owns this folder — remove it with `claude plugin`, or Threadknot would only see it return on the next update."
                  >
                    managed by a plugin
                  </span>
                ) : confirm === skill.id ? (
                  <div className="lib-confirm">
                    <span className="lib-confirm-label">remove from</span>
                    {SKILL_TARGETS.filter((t) => skill.targets.includes(t.id)).map(
                      (t) => (
                        <button
                          key={t.id}
                          type="button"
                          className="lib-mini-btn danger"
                          onClick={() => {
                            setConfirm(null);
                            void run(`rm:${skill.id}`, () =>
                              actions.removeSkill(skill.id, [t.id], machine),
                            );
                          }}
                        >
                          {t.label}
                        </button>
                      ),
                    )}
                    {skill.targets.length > 1 && (
                      <button
                        type="button"
                        className="lib-mini-btn danger"
                        onClick={() => {
                          setConfirm(null);
                          void run(`rm:${skill.id}`, () =>
                            actions.removeSkill(skill.id, skill.targets, machine),
                          );
                        }}
                      >
                        everywhere
                      </button>
                    )}
                    <button
                      type="button"
                      className="lib-mini-btn"
                      onClick={() => setConfirm(null)}
                    >
                      cancel
                    </button>
                  </div>
                ) : (
                  <button
                    type="button"
                    className="lib-remove"
                    onClick={() => setConfirm(skill.id)}
                  >
                    Remove
                  </button>
                )}
              </div>
            </div>
          );
        })}
      </div>

      <div className="settings-block">
        <div className="settings-label">install for</div>
        <div className="lib-target-picker">
          {SKILL_TARGETS.map((t) => (
            <button
              key={t.id}
              type="button"
              className={`lib-target${targets.includes(t.id) ? " on" : ""}`}
              aria-pressed={targets.includes(t.id)}
              onClick={() => toggleTarget(t.id)}
            >
              {t.label}
            </button>
          ))}
        </div>
        <div className="bl-field-hint">
          Applies to everything you install below. All three read the same
          <code> SKILL.md</code> format.
        </div>
      </div>

      {categories.map((category) => (
        <div className="settings-block" key={category}>
          <div className="settings-label">{category.toLowerCase()}</div>
          <div className="lib-grid">
            {data.catalog.skills
              .filter((entry) => entry.category === category)
              .map((entry) => {
                const have = installed.has(entry.id);
                const key = `install:${entry.id}`;
                return (
                  <div key={entry.id} className={`lib-tile${have ? " have" : ""}`}>
                    <div className="lib-tile-head">
                      <span className="lib-tile-title">{entry.title}</span>
                      <span
                        className={`lib-tile-license${entry.source === "bundled" ? " bundled" : ""}`}
                        title={
                          entry.source === "bundled"
                            ? "Written by Threadknot and shipped inside the app — installs with no network"
                            : `Fetched from ${entry.repo} at install time`
                        }
                      >
                        {entry.source === "bundled" ? "built in" : entry.license}
                      </span>
                    </div>
                    <div className="lib-tile-desc">{entry.description}</div>
                    <div className="lib-tile-foot">
                      <a
                        className="lib-tile-link"
                        href={entry.homepage}
                        target="_blank"
                        rel="noreferrer"
                      >
                        source
                      </a>
                      <button
                        type="button"
                        className="lib-install-btn"
                        disabled={busy === key || targets.length === 0}
                        onClick={() =>
                          void run(key, () =>
                            actions.installSkill(
                              { agents: targets, catalogId: entry.id },
                              machine,
                            ),
                          )
                        }
                      >
                        {busy === key
                          ? "Installing…"
                          : have
                            ? "Reinstall"
                            : "Install"}
                      </button>
                    </div>
                  </div>
                );
              })}
          </div>
        </div>
      ))}

      <div className="settings-block">
        <div className="settings-label">install from a repository</div>
        <div className="bl-form">
          <label className="bl-field">
            <span className="bl-field-label">GitHub folder</span>
            <input
              className="bl-input mono"
              placeholder="https://github.com/owner/repo/tree/main/skills/thing"
              value={customSource}
              onChange={(e) => setCustomSource(e.target.value)}
            />
            <span className="bl-field-hint">
              Any public repository folder containing a <code>SKILL.md</code>.
              Threadknot downloads the files — nothing is executed during install.
            </span>
          </label>
          <button
            type="button"
            className="bl-create-btn"
            disabled={busy === "custom" || !customSource.trim() || targets.length === 0}
            onClick={() =>
              void run("custom", async () => {
                await actions.installSkill(
                  { agents: targets, source: customSource.trim() },
                  machine,
                );
                setCustomSource("");
              })
            }
          >
            {busy === "custom" ? "Installing…" : "Install from URL"}
          </button>
        </div>
      </div>

      {error && <div className="settings-value settings-error">{error}</div>}
    </>
  );
}

/* -------------------------------------------------------------------------- */
/* MCP servers                                                                 */
/* -------------------------------------------------------------------------- */

function McpShelf({
  data,
  machine,
  reload,
}: {
  data: LibraryData;
  machine?: string;
  reload: () => void;
}) {
  const { actions } = useStore();
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [confirm, setConfirm] = useState<string | null>(null);
  /** Catalog entry whose input form is open. */
  const [pending, setPending] = useState<CatalogMcp | null>(null);
  const [inputs, setInputs] = useState<Record<string, string>>({});
  const [manual, setManual] = useState(false);

  const run = async (key: string, fn: () => Promise<unknown>) => {
    setBusy(key);
    setError("");
    try {
      await fn();
      reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy("");
    }
  };

  const installed = useMemo(
    () => new Set(data.mcpServers.map((s) => s.catalogId).filter(Boolean)),
    [data.mcpServers],
  );

  const categories = useMemo(() => {
    const seen: string[] = [];
    for (const entry of data.catalog.mcp) {
      if (!seen.includes(entry.category)) seen.push(entry.category);
    }
    return seen;
  }, [data.catalog.mcp]);

  const begin = (entry: CatalogMcp) => {
    if (entry.inputs.length === 0) {
      void run(`install:${entry.id}`, () =>
        actions.installMcpServer({ catalogId: entry.id }, machine),
      );
      return;
    }
    setPending(entry);
    setInputs({});
  };

  return (
    <>
      <div className="settings-block">
        <div className="settings-label">how MCP servers work</div>
        <div className="lib-note">
          An MCP server is a set of tools the agent can call. Threadknot hands every
          enabled server to Claude, Codex and Kimi as each session starts, so a
          server added here is available in every new chat — alongside Threadknot’s
          own browser tools, which cannot be displaced.
          <strong>
            {" "}
            A local server is a program launched with Threadknot’s privileges, and a
            remote one is usually holding a token of yours.
          </strong>{" "}
          Install what you would run yourself.
        </div>
      </div>

      <div className="settings-block">
        <div className="settings-label">installed</div>
        {data.mcpServers.length === 0 && (
          <div className="bl-empty">
            No MCP servers yet. The catalog below covers the official reference
            set plus a few hosted ones.
          </div>
        )}
        {data.mcpServers.map((server) => (
          <div key={server.id} className={`lib-card${server.enabled ? "" : " off"}`}>
            <div className="lib-card-head">
              <div className="lib-card-title">
                {server.label || server.name}
                <code className="lib-ns">mcp__{server.name}__*</code>
              </div>
              <div className="lib-badges">
                <span className={`lib-badge ${server.transport.type}`}>
                  {server.transport.type === "http" ? "remote" : "local"}
                </span>
                {server.agents.length === 0 ? (
                  <span className="lib-badge all">all agents</span>
                ) : (
                  server.agents.map((a) => (
                    <span key={a} className="lib-badge">
                      {a}
                    </span>
                  ))
                )}
              </div>
            </div>
            {server.description && (
              <div className="lib-card-desc">{server.description}</div>
            )}
            <div className="lib-card-meta">
              <span className="lib-meta-path" title={transportSummary(server.transport)}>
                {transportSummary(server.transport)}
              </span>
            </div>
            <div className="lib-card-actions">
              <button
                type="button"
                className="lib-mini-btn"
                disabled={busy === `toggle:${server.id}`}
                onClick={() =>
                  void run(`toggle:${server.id}`, () =>
                    actions.saveMcpServer(
                      { ...server, enabled: !server.enabled },
                      machine,
                    ),
                  )
                }
              >
                {server.enabled ? "Disable" : "Enable"}
              </button>
              {confirm === server.id ? (
                <>
                  <button
                    type="button"
                    className="lib-mini-btn danger"
                    onClick={() => {
                      setConfirm(null);
                      void run(`rm:${server.id}`, () =>
                        actions.deleteMcpServer(server.id, machine),
                      );
                    }}
                  >
                    Really remove
                  </button>
                  <button
                    type="button"
                    className="lib-mini-btn"
                    onClick={() => setConfirm(null)}
                  >
                    cancel
                  </button>
                </>
              ) : (
                <button
                  type="button"
                  className="lib-remove"
                  onClick={() => setConfirm(server.id)}
                >
                  Remove
                </button>
              )}
            </div>
          </div>
        ))}
        <div className="bl-field-hint">
          Changes apply to chats started from now on — a session already running
          keeps the servers it launched with.
        </div>
      </div>

      {pending && (
        <div className="settings-block">
          <div className="settings-label">set up {pending.title}</div>
          <div className="bl-form">
            {pending.inputs.map((input) => (
              <label className="bl-field" key={input.key}>
                <span className="bl-field-label">
                  {input.label}
                  {input.optional && <em> optional</em>}
                </span>
                <input
                  className="bl-input mono"
                  type={input.secret ? "password" : "text"}
                  placeholder={input.placeholder}
                  value={inputs[input.key] ?? ""}
                  onChange={(e) =>
                    setInputs((current) => ({
                      ...current,
                      [input.key]: e.target.value,
                    }))
                  }
                />
                {input.help && <span className="bl-field-hint">{input.help}</span>}
              </label>
            ))}
            <div className="lib-card-actions">
              <button
                type="button"
                className="bl-create-btn"
                disabled={busy === `install:${pending.id}`}
                onClick={() =>
                  void run(`install:${pending.id}`, async () => {
                    await actions.installMcpServer(
                      { catalogId: pending.id, inputs },
                      machine,
                    );
                    setPending(null);
                    setInputs({});
                  })
                }
              >
                {busy === `install:${pending.id}` ? "Installing…" : "Install"}
              </button>
              <button
                type="button"
                className="lib-mini-btn"
                onClick={() => setPending(null)}
              >
                cancel
              </button>
            </div>
          </div>
        </div>
      )}

      {categories.map((category) => (
        <div className="settings-block" key={category}>
          <div className="settings-label">{category.toLowerCase()}</div>
          <div className="lib-grid">
            {data.catalog.mcp
              .filter((entry) => entry.category === category)
              .map((entry) => {
                const have = installed.has(entry.id);
                const key = `install:${entry.id}`;
                return (
                  <div key={entry.id} className={`lib-tile${have ? " have" : ""}`}>
                    <div className="lib-tile-head">
                      <span className="lib-tile-title">{entry.title}</span>
                      {entry.prereq && (
                        <span
                          className={`lib-prereq${entry.prereqOk ? " ok" : ""}`}
                          title={
                            entry.prereqOk
                              ? `${entry.prereq} is on this machine's PATH`
                              : `${entry.prereq} was not found on this machine — the server would fail to start`
                          }
                        >
                          {entry.prereq}
                          {entry.prereqOk ? " ✓" : " missing"}
                        </span>
                      )}
                    </div>
                    <div className="lib-tile-desc">{entry.description}</div>
                    <div className="lib-tile-foot">
                      <a
                        className="lib-tile-link"
                        href={entry.homepage}
                        target="_blank"
                        rel="noreferrer"
                      >
                        source
                      </a>
                      <button
                        type="button"
                        className="lib-install-btn"
                        disabled={busy === key}
                        onClick={() => begin(entry)}
                      >
                        {busy === key ? "Installing…" : have ? "Add another" : "Install"}
                      </button>
                    </div>
                  </div>
                );
              })}
          </div>
        </div>
      ))}

      <div className="settings-block">
        <div className="settings-label">add one by hand</div>
        {manual ? (
          <ManualServerForm
            busy={busy === "manual"}
            onCancel={() => setManual(false)}
            onSave={(server) =>
              void run("manual", async () => {
                await actions.saveMcpServer(server, machine);
                setManual(false);
              })
            }
          />
        ) : (
          <button type="button" className="bl-create-btn" onClick={() => setManual(true)}>
            Add a server
          </button>
        )}
      </div>

      {error && <div className="settings-value settings-error">{error}</div>}
    </>
  );
}

/**
 * Free-form server entry. Kept simple on purpose: a name, a transport, and the
 * one field each transport actually needs. Anything more elaborate is better
 * expressed as a catalog entry.
 */
function ManualServerForm({
  busy,
  onSave,
  onCancel,
}: {
  busy: boolean;
  onSave: (server: McpServerInfo) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState("");
  const [kind, setKind] = useState<"stdio" | "http">("stdio");
  const [command, setCommand] = useState("");
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");

  // `npx -y pkg --flag` → command `npx`, args the rest. Quoting is not
  // supported; a path with spaces belongs in a catalog entry, not this box.
  const parts = command.trim().split(/\s+/).filter(Boolean);

  const build = (): McpServerInfo => ({
    id: "",
    name: name.trim(),
    label: "",
    description: "",
    transport:
      kind === "http"
        ? {
            type: "http",
            url: url.trim(),
            headers: token.trim() ? { Authorization: `Bearer ${token.trim()}` } : {},
          }
        : { type: "stdio", command: parts[0] ?? "", args: parts.slice(1), env: {} },
    enabled: true,
    agents: [],
    createdAt: "",
  });

  const ready =
    name.trim().length > 0 && (kind === "http" ? url.trim().length > 0 : parts.length > 0);

  return (
    <div className="bl-form">
      <label className="bl-field">
        <span className="bl-field-label">Name</span>
        <input
          className="bl-input mono"
          placeholder="my-server"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <span className="bl-field-hint">
          Becomes the agent’s tool prefix: <code>mcp__{name || "my-server"}__*</code>.
          Letters, numbers, dashes and underscores.
        </span>
      </label>
      <div className="lib-target-picker">
        <button
          type="button"
          className={`lib-target${kind === "stdio" ? " on" : ""}`}
          aria-pressed={kind === "stdio"}
          onClick={() => setKind("stdio")}
        >
          Local command
        </button>
        <button
          type="button"
          className={`lib-target${kind === "http" ? " on" : ""}`}
          aria-pressed={kind === "http"}
          onClick={() => setKind("http")}
        >
          Remote URL
        </button>
      </div>
      {kind === "stdio" ? (
        <label className="bl-field">
          <span className="bl-field-label">Command</span>
          <input
            className="bl-input mono"
            placeholder="npx -y @scope/some-mcp-server"
            value={command}
            onChange={(e) => setCommand(e.target.value)}
          />
        </label>
      ) : (
        <>
          <label className="bl-field">
            <span className="bl-field-label">URL</span>
            <input
              className="bl-input mono"
              placeholder="https://example.com/mcp"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
          </label>
          <label className="bl-field">
            <span className="bl-field-label">
              Bearer token <em>optional</em>
            </span>
            <input
              className="bl-input mono"
              type="password"
              value={token}
              onChange={(e) => setToken(e.target.value)}
            />
            <span className="bl-field-hint">
              Sent as <code>Authorization: Bearer …</code>. Codex only supports
              this header shape; Claude and Kimi accept any header, which you can
              add later by editing <code>~/.threadknot/mcp-servers.json</code>.
            </span>
          </label>
        </>
      )}
      <div className="lib-card-actions">
        <button
          type="button"
          className="bl-create-btn"
          disabled={busy || !ready}
          onClick={() => onSave(build())}
        >
          {busy ? "Saving…" : "Save server"}
        </button>
        <button type="button" className="lib-mini-btn" onClick={onCancel}>
          cancel
        </button>
      </div>
    </div>
  );
}
