import { useCallback, useEffect, useRef, useState } from "react";
import type {
  GitBranchesData,
  GitDiffScope,
  GitFileEntry,
  GitOpResult,
  GitRepoInfo,
  GitStatusData,
  Project,
} from "../lib/protocol";
import { useStore } from "../state/store";
import {
  ChevronIcon,
  DownloadCloudIcon,
  GitBranchIcon,
  UploadIcon,
} from "./icons";
import "../styles/git.css";

/** Porcelain status letter → human label. */
function statusLabel(c: string): string {
  switch (c) {
    case "M": return "modified";
    case "T": return "typechange";
    case "A": return "added";
    case "D": return "deleted";
    case "R": return "renamed";
    case "C": return "copied";
    case "U": return "conflict";
    case "?": return "untracked";
    default: return c;
  }
}

function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Sections of the drill-in status list. An entry with both staged and
 *  worktree changes (e.g. "MM") appears in both Staged and Changes. */
function splitEntries(entries: GitFileEntry[]) {
  const conflicted = entries.filter((e) => e.kind === "conflicted");
  const staged = entries.filter((e) => e.kind === "changed" && e.x !== ".");
  const changed = entries.filter((e) => e.kind === "changed" && e.y !== ".");
  const untracked = entries.filter((e) => e.kind === "untracked");
  return { conflicted, staged, changed, untracked };
}

// ---- diff viewer ---------------------------------------------------------

function DiffView({
  repoId,
  path,
  scope,
  onBack,
}: {
  repoId: string;
  path: string;
  scope: GitDiffScope;
  onBack: () => void;
}) {
  const { actions } = useStore();
  const [unified, setUnified] = useState<string | null>(null);
  const [meta, setMeta] = useState<{ truncated: boolean; binary: boolean } | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let gone = false;
    setUnified(null);
    setError(null);
    actions
      .gitDiff(repoId, path, scope)
      .then((d) => {
        if (gone) return;
        setUnified(d.unified);
        setMeta({ truncated: d.truncated, binary: d.binary });
      })
      .catch((e) => !gone && setError(errText(e)));
    return () => {
      gone = true;
    };
  }, [actions, repoId, path, scope]);

  return (
    <div className="git-pane">
      <header className="git-head">
        <div className="git-head-top">
          <button type="button" className="git-back" onClick={onBack}>
            <ChevronIcon size={12} className="git-back-chev" /> Back
          </button>
          <span className="git-diff-path" title={path}>{path}</span>
          {scope === "staged" && <span className="git-chip stage">staged</span>}
          {meta?.truncated && <span className="git-chip warn">truncated</span>}
        </div>
      </header>
      <div className="git-scroll">
        {error && <div className="git-empty git-error">{error}</div>}
        {!error && unified === null && <div className="git-empty">Loading…</div>}
        {!error && meta?.binary && <div className="git-empty">Binary file.</div>}
        {!error && unified !== null && !meta?.binary && unified.length === 0 && (
          <div className="git-empty">No changes.</div>
        )}
        {!error && unified && !meta?.binary && (
          <pre className="git-diff-body">
            {unified.split("\n").map((line, i) => {
              let cls = "ctx";
              if (line.startsWith("+++") || line.startsWith("---")) cls = "file";
              else if (line.startsWith("@@")) cls = "hunk";
              else if (line.startsWith("+")) cls = "add";
              else if (line.startsWith("-")) cls = "del";
              return (
                <span key={i} className={`git-diff-line ${cls}`}>
                  {line || " "}
                </span>
              );
            })}
          </pre>
        )}
      </div>
    </div>
  );
}

// ---- branch picker -------------------------------------------------------

/** Themed branch dropdown (native <select> popups ignore the app theme).
 *  Follows the AgentSelect pattern: trigger button + absolute menu, outside
 *  click closes. Long lists get a filter box; remote-only branches are offered
 *  too (checking one out creates the tracking branch). */
function BranchSelect({
  current,
  branches,
  remoteBranches,
  disabled,
  onSelect,
}: {
  current: string;
  branches: string[];
  remoteBranches: string[];
  disabled: boolean;
  onSelect: (value: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    function onDown(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const q = query.trim().toLowerCase();
  const local = branches.filter((b) => !q || b.toLowerCase().includes(q));
  const remote = remoteBranches.filter((b) => !q || b.toLowerCase().includes(q));
  const showFilter = branches.length + remoteBranches.length > 8;
  // Autofocusing the filter would pop the on-screen keyboard on touch devices.
  const autoFocus = !window.matchMedia("(pointer: coarse)").matches;

  const pick = (value: string) => {
    setOpen(false);
    setQuery("");
    onSelect(value);
  };

  return (
    <div className="git-branch-dd" ref={ref}>
      <button
        type="button"
        className="git-branch-trigger"
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="git-branch-cur">{current}</span>
        <ChevronIcon size={11} open={open} className="row-chevron" />
      </button>
      {open && (
        <div className="git-branch-menu" role="listbox">
          {showFilter && (
            <input
              className="git-branch-filter"
              placeholder="Filter branches…"
              value={query}
              autoFocus={autoFocus}
              spellCheck={false}
              autoCapitalize="off"
              autoCorrect="off"
              onChange={(e) => setQuery(e.target.value)}
            />
          )}
          <div className="git-branch-list">
            {local.length > 0 && <div className="git-branch-group">Local</div>}
            {local.map((b) => (
              <button
                key={b}
                type="button"
                role="option"
                aria-selected={b === current}
                className={`git-branch-opt${b === current ? " on" : ""}`}
                onClick={() => pick(b)}
              >
                {b}
              </button>
            ))}
            {remote.length > 0 && <div className="git-branch-group">Remote</div>}
            {remote.map((b) => (
              <button
                key={`r:${b}`}
                type="button"
                role="option"
                aria-selected={false}
                className="git-branch-opt"
                onClick={() => pick(b)}
              >
                {b}
              </button>
            ))}
            {local.length === 0 && remote.length === 0 && (
              <div className="git-branch-none">No matches.</div>
            )}
          </div>
          <button type="button" className="git-branch-opt new" onClick={() => pick("__new__")}>
            + new branch…
          </button>
        </div>
      )}
    </div>
  );
}

// ---- one repo (drill-in) -------------------------------------------------

/** Path rendered as dim directory + bright filename so the part that matters
 *  stays readable; the directory truncates first when space runs out. */
function PathLabel({ path }: { path: string }) {
  const idx = path.lastIndexOf("/");
  const dir = idx >= 0 ? path.slice(0, idx + 1) : "";
  const base = idx >= 0 ? path.slice(idx + 1) : path;
  return (
    <span className="git-row-path">
      {dir && <span className="git-path-dir">{dir}</span>}
      <span className="git-path-base">{base}</span>
    </span>
  );
}

function FileRow({
  entry,
  letter,
  actionsSlot,
  onOpen,
}: {
  entry: GitFileEntry;
  letter: string;
  actionsSlot: React.ReactNode;
  onOpen: () => void;
}) {
  const title = entry.origPath ? `${entry.origPath} → ${entry.path}` : entry.path;
  return (
    <div className="git-row">
      <button type="button" className="git-row-main" onClick={onOpen} title={title}>
        <span className={`git-letter s-${letter}`} title={statusLabel(letter)}>{letter}</span>
        <PathLabel path={entry.path} />
      </button>
      <span className="git-row-actions">{actionsSlot}</span>
    </div>
  );
}

function RepoView({
  repo,
  showBack,
  onBack,
}: {
  repo: GitRepoInfo;
  showBack: boolean;
  onBack: () => void;
}) {
  const { actions } = useStore();
  const [status, setStatus] = useState<GitStatusData | null>(null);
  const [branches, setBranches] = useState<GitBranchesData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [opError, setOpError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [diff, setDiff] = useState<{ path: string; scope: GitDiffScope } | null>(null);
  const [prUrl, setPrUrl] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [st, br] = await Promise.all([
        actions.gitStatus(repo.id),
        actions.gitBranches(repo.id).catch(() => null),
      ]);
      setStatus(st);
      if (br) setBranches(br);
      setError(null);
    } catch (e) {
      setError(errText(e));
    }
  }, [actions, repo.id]);

  useEffect(() => {
    void load();
    // `repo` (the fleet summary) is refreshed on turn completion / broadcasts;
    // re-pulling detailed status here keeps the drill-in view in sync too.
  }, [load, repo]);

  /** Run a mutation; the server answers with the fresh status. */
  const run = useCallback(
    async (op: () => Promise<GitStatusData>) => {
      setBusy(true);
      setOpError(null);
      try {
        setStatus(await op());
      } catch (e) {
        setOpError(errText(e));
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  if (diff) {
    return (
      <DiffView
        repoId={repo.id}
        path={diff.path}
        scope={diff.scope}
        onBack={() => setDiff(null)}
      />
    );
  }

  const sections = status ? splitEntries(status.entries) : null;
  const clean = status !== null && status.entries.length === 0;

  const checkoutBranch = (value: string) => {
    if (value === "__new__") {
      const name = window.prompt("New branch name:")?.trim();
      if (!name) return;
      void run(() => actions.gitCheckout(repo.id, name, true)).then(() =>
        actions.gitBranches(repo.id).then(setBranches).catch(() => undefined),
      );
    } else if (status && value !== status.branch) {
      void run(() => actions.gitCheckout(repo.id, value)).then(() =>
        actions.gitBranches(repo.id).then(setBranches).catch(() => undefined),
      );
    }
  };

  const discard = (paths: string[], untracked: boolean) => {
    const what = paths.length === 1 ? paths[0] : `${paths.length} files`;
    const verb = untracked ? "Delete" : "Discard changes to";
    if (!window.confirm(`${verb} ${what}? This cannot be undone.`)) return;
    void run(() => actions.gitDiscard(repo.id, paths));
  };

  return (
    <div className="git-pane">
      <header className="git-head">
        <div className="git-head-top">
          {showBack && (
            <button type="button" className="git-back" onClick={onBack}>
              <ChevronIcon size={12} className="git-back-chev" /> Repos
            </button>
          )}
          <span className="git-repo-name" title={repo.relPath || "."}>{repo.name}</span>
          <button
            type="button"
            className="git-refresh"
            onClick={() => void load()}
            disabled={busy}
            title="Refresh"
          >
            Refresh
          </button>
        </div>
        {status && (
          <div className="git-head-branch">
            <GitBranchIcon size={13} />
            {branches && !status.detached ? (
              <BranchSelect
                current={status.branch}
                branches={branches.branches}
                remoteBranches={branches.remoteBranches ?? []}
                disabled={busy}
                onSelect={checkoutBranch}
              />
            ) : (
              <span className="git-branch-name">{status.branch}</span>
            )}
            <span className="git-head-spacer" />
            <button
              type="button"
              className="git-sync"
              onClick={() => void run(() => actions.gitPull(repo.id))}
              disabled={busy}
              title="git pull --ff-only"
            >
              <DownloadCloudIcon size={13} />
              {status.behind > 0 ? ` ${status.behind}` : ""}
            </button>
            <button
              type="button"
              className="git-sync"
              onClick={() => void run(() => actions.gitPush(repo.id))}
              disabled={busy}
              title="git push"
            >
              <UploadIcon size={13} />
              {status.ahead > 0 ? ` ${status.ahead}` : ""}
            </button>
            <button
              type="button"
              className="git-sync"
              onClick={() => {
                setBusy(true);
                setOpError(null);
                actions
                  .gitPr(repo.id)
                  .then((d) => {
                    setPrUrl(d.url ?? null);
                    if (!d.url) setOpError(d.output || "gh returned no PR URL");
                  })
                  .catch((e) => setOpError(errText(e)))
                  .finally(() => setBusy(false));
              }}
              disabled={busy}
              title="Create a pull request for this branch (gh pr create)"
            >
              PR
            </button>
          </div>
        )}
        {prUrl && (
          <div className="git-pr-link">
            <a href={prUrl} target="_blank" rel="noreferrer">{prUrl}</a>
          </div>
        )}
      </header>

      <div className="git-scroll">
        {error && <div className="git-empty git-error">{error}</div>}
        {opError && (
          <div className="git-op-error">
            <span>{opError}</span>
            <button type="button" onClick={() => setOpError(null)} aria-label="Dismiss">×</button>
          </div>
        )}
        {!error && status === null && <div className="git-empty">Loading…</div>}
        {clean && <div className="git-empty">Working tree clean.</div>}

        {sections && sections.conflicted.length > 0 && (
          <section className="git-section">
            <h3 className="git-section-title conflict">Conflicts ({sections.conflicted.length})</h3>
            {sections.conflicted.map((e) => (
              <FileRow
                key={`u:${e.path}`}
                entry={e}
                letter="U"
                actionsSlot={null}
                onOpen={() => setDiff({ path: e.path, scope: "worktree" })}
              />
            ))}
          </section>
        )}

        {sections && sections.staged.length > 0 && (
          <section className="git-section">
            <h3 className="git-section-title">
              Staged ({sections.staged.length})
              <button
                type="button"
                className="git-mini"
                disabled={busy}
                onClick={() =>
                  void run(() =>
                    actions.gitUnstage(repo.id, sections.staged.map((e) => e.path)),
                  )
                }
              >
                Unstage all
              </button>
            </h3>
            {sections.staged.map((e) => (
              <FileRow
                key={`s:${e.path}`}
                entry={e}
                letter={e.x}
                onOpen={() => setDiff({ path: e.path, scope: "staged" })}
                actionsSlot={
                  <button
                    type="button"
                    className="git-mini"
                    disabled={busy}
                    onClick={() => void run(() => actions.gitUnstage(repo.id, [e.path]))}
                  >
                    Unstage
                  </button>
                }
              />
            ))}
          </section>
        )}

        {sections && sections.changed.length > 0 && (
          <section className="git-section">
            <h3 className="git-section-title">
              Changes ({sections.changed.length})
              <button
                type="button"
                className="git-mini"
                disabled={busy}
                onClick={() =>
                  void run(() =>
                    actions.gitStage(repo.id, sections.changed.map((e) => e.path)),
                  )
                }
              >
                Stage all
              </button>
            </h3>
            {sections.changed.map((e) => (
              <FileRow
                key={`c:${e.path}`}
                entry={e}
                letter={e.y}
                onOpen={() => setDiff({ path: e.path, scope: "worktree" })}
                actionsSlot={
                  <>
                    <button
                      type="button"
                      className="git-mini"
                      disabled={busy}
                      onClick={() => void run(() => actions.gitStage(repo.id, [e.path]))}
                    >
                      Stage
                    </button>
                    <button
                      type="button"
                      className="git-mini danger"
                      disabled={busy}
                      onClick={() => discard([e.path], false)}
                    >
                      Discard
                    </button>
                  </>
                }
              />
            ))}
          </section>
        )}

        {sections && sections.untracked.length > 0 && (
          <section className="git-section">
            <h3 className="git-section-title">
              Untracked ({sections.untracked.length})
              <button
                type="button"
                className="git-mini"
                disabled={busy}
                onClick={() =>
                  void run(() =>
                    actions.gitStage(repo.id, sections.untracked.map((e) => e.path)),
                  )
                }
              >
                Stage all
              </button>
            </h3>
            {sections.untracked.map((e) => (
              <FileRow
                key={`n:${e.path}`}
                entry={e}
                letter="?"
                onOpen={() => setDiff({ path: e.path, scope: "untracked" })}
                actionsSlot={
                  <>
                    <button
                      type="button"
                      className="git-mini"
                      disabled={busy}
                      onClick={() => void run(() => actions.gitStage(repo.id, [e.path]))}
                    >
                      Stage
                    </button>
                    <button
                      type="button"
                      className="git-mini danger"
                      disabled={busy}
                      onClick={() => discard([e.path], true)}
                    >
                      Delete
                    </button>
                  </>
                }
              />
            ))}
          </section>
        )}
      </div>

      {status && (
        <footer className="git-commit">
          <textarea
            className="git-commit-msg"
            placeholder={
              status.staged > 0
                ? `Commit message for ${status.staged} staged file${status.staged === 1 ? "" : "s"}…`
                : "Stage files to commit…"
            }
            value={message}
            rows={2}
            disabled={busy}
            onChange={(e) => setMessage(e.target.value)}
          />
          <button
            type="button"
            className="git-commit-btn"
            disabled={busy || status.staged === 0 || message.trim().length === 0}
            onClick={() =>
              void run(async () => {
                const out = await actions.gitCommit(repo.id, message.trim());
                setMessage("");
                return out;
              })
            }
          >
            Commit
          </button>
        </footer>
      )}
    </div>
  );
}

// ---- cross-repo sheets ---------------------------------------------------

function SheetHeader({ title, onBack }: { title: string; onBack: () => void }) {
  return (
    <header className="git-head">
      <div className="git-head-top">
        <button type="button" className="git-back" onClick={onBack}>
          <ChevronIcon size={12} className="git-back-chev" /> Repos
        </button>
        <span className="git-repo-name">{title}</span>
      </div>
    </header>
  );
}

function ResultLine({ result }: { result: GitOpResult | undefined }) {
  if (!result) return null;
  if (!result.ok) return <div className="git-sheet-result err">{result.error}</div>;
  if (result.hash)
    return (
      <div className="git-sheet-result ok">
        committed <span className="git-card-hash">{result.hash}</span> {result.subject}
      </div>
    );
  return (
    <div className="git-sheet-result ok">
      {result.created ? "branch created" : "switched"}
    </div>
  );
}

/** Commit every dirty repo in one action: per-repo message (prefilled from the
 *  changed file names), stage-all semantics, optional linked-changeset trailer. */
function CommitSheet({
  project,
  repos,
  onBack,
}: {
  project: Project;
  repos: GitRepoInfo[];
  onBack: () => void;
}) {
  const { actions } = useStore();
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [excluded, setExcluded] = useState<Set<string>>(new Set());
  const [link, setLink] = useState(true);
  const [busy, setBusy] = useState(false);
  const [results, setResults] = useState<Record<string, GitOpResult>>({});
  const [error, setError] = useState<string | null>(null);

  // Prefill messages from each repo's changed file names; anything the user
  // already typed wins over a late-arriving prefill.
  useEffect(() => {
    let gone = false;
    void Promise.all(
      repos.map(async (r) => {
        try {
          const st = await actions.gitStatus(r.id);
          const names = st.entries.map((e) => e.path.split("/").pop() ?? e.path);
          const head = names.slice(0, 3).join(", ");
          const extra = names.length > 3 ? ` +${names.length - 3} more` : "";
          return [r.id, names.length ? `Update ${head}${extra}` : ""] as const;
        } catch {
          return [r.id, ""] as const;
        }
      }),
    ).then((pairs) => {
      if (!gone) setDrafts((prev) => ({ ...Object.fromEntries(pairs), ...prev }));
    });
    return () => {
      gone = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [actions]);

  const included = repos.filter(
    (r) => !excluded.has(r.id) && (drafts[r.id] ?? "").trim().length > 0,
  );

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const { results: out } = await actions.gitCommitMany({
        projectId: project.id,
        entries: included.map((r) => ({
          repoId: r.id,
          message: drafts[r.id].trim(),
          stageAll: true,
        })),
        link,
      });
      setResults(Object.fromEntries(out.map((res) => [res.repoId, res])));
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const done = Object.keys(results).length > 0;

  return (
    <div className="git-pane">
      <SheetHeader title="Commit all" onBack={onBack} />
      <div className="git-scroll">
        {error && <div className="git-op-error"><span>{error}</span></div>}
        <p className="git-sheet-hint">
          Stages everything in each selected repo and commits it with the message below.
        </p>
        {repos.map((r) => (
          <div key={r.id} className={`git-sheet-row${excluded.has(r.id) ? " off" : ""}`}>
            <label className="git-sheet-repo">
              <input
                type="checkbox"
                checked={!excluded.has(r.id)}
                disabled={busy || done}
                onChange={(e) =>
                  setExcluded((prev) => {
                    const next = new Set(prev);
                    if (e.target.checked) next.delete(r.id);
                    else next.add(r.id);
                    return next;
                  })
                }
              />
              <span className="git-card-name">{r.name}</span>
              <span className="git-card-state">
                {(r.staged ?? 0) > 0 && <span className="git-chip stage">{r.staged} staged</span>}
                {(r.unstaged ?? 0) > 0 && <span className="git-chip change">{r.unstaged} changed</span>}
                {(r.untracked ?? 0) > 0 && <span className="git-chip new">{r.untracked} new</span>}
              </span>
            </label>
            <textarea
              className="git-commit-msg"
              rows={2}
              value={drafts[r.id] ?? ""}
              placeholder="Commit message…"
              disabled={busy || done || excluded.has(r.id)}
              onChange={(e) => setDrafts((prev) => ({ ...prev, [r.id]: e.target.value }))}
            />
            <ResultLine result={results[r.id]} />
          </div>
        ))}
      </div>
      <footer className="git-commit git-sheet-foot">
        {!done && (
          <label className="git-sheet-link" title="Stamp each commit with a shared Threadknot-Change trailer">
            <input
              type="checkbox"
              checked={link}
              disabled={busy || included.length < 2}
              onChange={(e) => setLink(e.target.checked)}
            />
            Link commits
          </label>
        )}
        {done ? (
          <button type="button" className="git-commit-btn" onClick={onBack}>
            Done
          </button>
        ) : (
          <button
            type="button"
            className="git-commit-btn"
            disabled={busy || included.length === 0}
            onClick={() => void submit()}
          >
            Commit {included.length} repo{included.length === 1 ? "" : "s"}
          </button>
        )}
      </footer>
    </div>
  );
}

/** Create-or-switch the same branch across selected repos. */
function BranchSheet({
  project,
  repos,
  onBack,
}: {
  project: Project;
  repos: GitRepoInfo[];
  onBack: () => void;
}) {
  const { actions } = useStore();
  const [name, setName] = useState("");
  const [excluded, setExcluded] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [results, setResults] = useState<Record<string, GitOpResult>>({});
  const [error, setError] = useState<string | null>(null);

  const included = repos.filter((r) => !excluded.has(r.id));
  const done = Object.keys(results).length > 0;

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const { results: out } = await actions.gitCheckoutMany(
        project.id,
        included.map((r) => r.id),
        name.trim(),
      );
      setResults(Object.fromEntries(out.map((res) => [res.repoId, res])));
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="git-pane">
      <SheetHeader title="Branch across repos" onBack={onBack} />
      <div className="git-scroll">
        {error && <div className="git-op-error"><span>{error}</span></div>}
        <p className="git-sheet-hint">
          Switches every selected repo to this branch, creating it where it doesn't exist.
        </p>
        <input
          type="text"
          className="git-branch-input"
          placeholder="branch name (e.g. feat/checkout-flow)"
          value={name}
          disabled={busy || done}
          spellCheck={false}
          autoCapitalize="off"
          onChange={(e) => setName(e.target.value)}
        />
        {repos.map((r) => (
          <div key={r.id} className={`git-sheet-row${excluded.has(r.id) ? " off" : ""}`}>
            <label className="git-sheet-repo">
              <input
                type="checkbox"
                checked={!excluded.has(r.id)}
                disabled={busy || done}
                onChange={(e) =>
                  setExcluded((prev) => {
                    const next = new Set(prev);
                    if (e.target.checked) next.delete(r.id);
                    else next.add(r.id);
                    return next;
                  })
                }
              />
              <span className="git-card-name">{r.name}</span>
              <span className="git-card-branch">
                <GitBranchIcon size={11} />
                {r.branch}
              </span>
            </label>
            <ResultLine result={results[r.id]} />
          </div>
        ))}
      </div>
      <footer className="git-commit git-sheet-foot">
        {done ? (
          <button type="button" className="git-commit-btn" onClick={onBack}>
            Done
          </button>
        ) : (
          <button
            type="button"
            className="git-commit-btn"
            disabled={busy || included.length === 0 || name.trim().length === 0}
            onClick={() => void submit()}
          >
            Switch {included.length} repo{included.length === 1 ? "" : "s"}
          </button>
        )}
      </footer>
    </div>
  );
}

// ---- fleet overview ------------------------------------------------------

function RepoCard({ repo, onOpen }: { repo: GitRepoInfo; onOpen: () => void }) {
  const dirty = (repo.staged ?? 0) + (repo.unstaged ?? 0) + (repo.untracked ?? 0);
  return (
    <button type="button" className="git-card" onClick={onOpen} disabled={!!repo.error}>
      <div className="git-card-top">
        <span className="git-card-name">{repo.name}</span>
        {repo.relPath && <span className="git-card-path">{repo.relPath}/</span>}
      </div>
      {repo.error ? (
        <div className="git-card-err">{repo.error}</div>
      ) : (
        <>
          <div className="git-card-branch">
            <GitBranchIcon size={12} />
            <span>{repo.branch}</span>
            {(repo.ahead ?? 0) > 0 && <span className="git-chip ahead">↑{repo.ahead}</span>}
            {(repo.behind ?? 0) > 0 && <span className="git-chip behind">↓{repo.behind}</span>}
          </div>
          <div className="git-card-state">
            {(repo.conflicted ?? 0) > 0 && (
              <span className="git-chip conflict">{repo.conflicted} conflict{repo.conflicted === 1 ? "" : "s"}</span>
            )}
            {(repo.staged ?? 0) > 0 && <span className="git-chip stage">{repo.staged} staged</span>}
            {(repo.unstaged ?? 0) > 0 && <span className="git-chip change">{repo.unstaged} changed</span>}
            {(repo.untracked ?? 0) > 0 && <span className="git-chip new">{repo.untracked} new</span>}
            {dirty === 0 && (repo.conflicted ?? 0) === 0 && <span className="git-chip clean">clean</span>}
          </div>
          {repo.lastCommit && (
            <div className="git-card-last" title={repo.lastCommit.subject}>
              <span className="git-card-hash">{repo.lastCommit.hash}</span> {repo.lastCommit.subject}
            </div>
          )}
        </>
      )}
    </button>
  );
}

/** Multi-repo Git tab: fleet overview (one card per repo found in the project
 *  folder) → drill into a repo for status/diff/stage/commit/branch/push/pull.
 *  A single-repo project skips straight to the repo view. */
export function GitPane({ project, active }: { project: Project; active: boolean }) {
  const { state, actions } = useStore();
  const repos = state.git[project.id];
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [sheet, setSheet] = useState<null | "commit" | "branch">(null);
  const [fleetBusy, setFleetBusy] = useState(false);
  /** Outcome line for pull-all / push-all (successes and per-repo errors). */
  const [fleetMsg, setFleetMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const loadedFor = useRef<string | null>(null);
  /** Set when the user explicitly went back — stops single-repo auto-drill. */
  const wentBack = useRef(false);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      await actions.refreshGitRepos(project.id);
    } catch (e) {
      setError(errText(e));
    } finally {
      setLoading(false);
    }
  }, [actions, project.id]);

  // Lazy-init on first `active`; refetch when the project changes.
  useEffect(() => {
    if (!active) return;
    if (loadedFor.current === project.id) return;
    loadedFor.current = project.id;
    setSelected(null);
    setSheet(null);
    setFleetMsg(null);
    wentBack.current = false;
    void load();
  }, [active, project.id, load]);

  // Single-repo projects go straight to the repo view (mono-repo ≙ N=1).
  useEffect(() => {
    if (!wentBack.current && selected === null && repos?.length === 1 && !repos[0].error) {
      setSelected(repos[0].id);
    }
  }, [repos, selected]);

  const selectedRepo = repos?.find((r) => r.id === selected) ?? null;
  if (selectedRepo) {
    return (
      <RepoView
        repo={selectedRepo}
        showBack={(repos?.length ?? 0) > 1}
        onBack={() => {
          wentBack.current = true;
          setSelected(null);
        }}
      />
    );
  }

  const healthy = (repos ?? []).filter((r) => !r.error);
  const dirty = healthy.filter(
    (r) => (r.staged ?? 0) + (r.unstaged ?? 0) + (r.untracked ?? 0) > 0,
  );
  const pullable = healthy.filter((r) => (r.behind ?? 0) > 0);
  const pushable = healthy.filter((r) => (r.ahead ?? 0) > 0);

  if (sheet === "commit") {
    return <CommitSheet project={project} repos={dirty} onBack={() => setSheet(null)} />;
  }
  if (sheet === "branch") {
    return <BranchSheet project={project} repos={healthy} onBack={() => setSheet(null)} />;
  }

  const syncAll = async (kind: "pull" | "push", targets: GitRepoInfo[]) => {
    setFleetBusy(true);
    setFleetMsg(null);
    const errs: string[] = [];
    for (const r of targets) {
      try {
        if (kind === "pull") await actions.gitPull(r.id);
        else await actions.gitPush(r.id);
      } catch (e) {
        errs.push(`${r.name}: ${errText(e)}`);
      }
    }
    await load().catch(() => undefined);
    setFleetMsg(
      errs.length > 0
        ? { ok: false, text: errs.join("\n") }
        : {
            ok: true,
            text: `${kind === "pull" ? "Pulled" : "Pushed"} ${targets.length} repo${targets.length === 1 ? "" : "s"}.`,
          },
    );
    setFleetBusy(false);
  };

  return (
    <div className="git-pane">
      <header className="git-head">
        <div className="git-head-top">
          <span className="git-count">
            {loading && !repos
              ? "Scanning…"
              : `${repos?.length ?? 0} repo${(repos?.length ?? 0) === 1 ? "" : "s"}`}
          </span>
          <button
            type="button"
            className="git-refresh"
            onClick={() => void load()}
            disabled={loading}
            title="Rescan for repos"
          >
            Refresh
          </button>
        </div>
        {healthy.length > 1 && (
          <div className="git-fleet-actions">
            <button
              type="button"
              className="git-mini"
              disabled={fleetBusy || dirty.length === 0}
              onClick={() => setSheet("commit")}
            >
              Commit all…
            </button>
            <button
              type="button"
              className="git-mini"
              disabled={fleetBusy}
              onClick={() => setSheet("branch")}
            >
              Branch…
            </button>
            <button
              type="button"
              className="git-mini"
              disabled={fleetBusy || pullable.length === 0}
              onClick={() => void syncAll("pull", pullable)}
            >
              Pull all{pullable.length > 0 ? ` (${pullable.length})` : ""}
            </button>
            <button
              type="button"
              className="git-mini"
              disabled={fleetBusy || pushable.length === 0}
              onClick={() => void syncAll("push", pushable)}
            >
              Push all{pushable.length > 0 ? ` (${pushable.length})` : ""}
            </button>
          </div>
        )}
      </header>
      <div className="git-scroll">
        {fleetMsg && (
          <div className={`git-op-error git-fleet-msg${fleetMsg.ok ? " ok" : ""}`}>
            <span>{fleetMsg.text}</span>
            <button type="button" onClick={() => setFleetMsg(null)} aria-label="Dismiss">×</button>
          </div>
        )}
        {error && <div className="git-empty git-error">{error}</div>}
        {!error && repos && repos.length === 0 && (
          <div className="git-empty">
            No git repositories found in this project folder.
          </div>
        )}
        {!error &&
          repos?.map((r) => (
            <RepoCard key={r.id} repo={r} onOpen={() => setSelected(r.id)} />
          ))}
      </div>
    </div>
  );
}
