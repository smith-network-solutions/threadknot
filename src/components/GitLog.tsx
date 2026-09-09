import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type RefObject,
} from "react";
import type {
  GitBranchesData,
  GitCommit,
  GitCommitDetails,
  GitCommitFile,
  GitRef,
  GitRepoInfo,
} from "../lib/protocol";
import { useStore } from "../state/store";
import { copyText, formatCompactDateTime, formatFullDateTime } from "../lib/format";
import { CheckIcon, ChevronIcon, CopyIcon, SearchIcon, XIcon } from "./icons";
import { DiffBody, PathLabel, errText, statusLabel, useDiff } from "./git/shared";

const PAGE = 200;
/** Pane width at which the commit and its files sit side by side. */
const WIDE_MIN = 640;
/** Pane width at which the author column fits beside the subject. */
const MID_MIN = 480;
const LANE_W = 12;
const ROW_H = 30;
/** Lanes past this are clipped rather than pushing the subject off-screen. */
const MAX_LANES = 6;
const LANE_COLORS = [
  "#d9a35c",
  "#7fb2d9",
  "#86c68c",
  "#b48ad9",
  "#d97ba8",
  "#5fc4bd",
  "#e0865f",
  "#9aa4b2",
];

// ---- graph layout --------------------------------------------------------

export interface GraphRow {
  /** Lane the commit's node sits in. */
  lane: number;
  color: number;
  /** Lines arriving from the row above that end at this node. */
  into: { lane: number; color: number }[];
  /** Lines leaving this node toward the row below (one per parent). */
  out: { lane: number; color: number }[];
  /** Lanes that pass straight through this row. */
  through: { lane: number; color: number }[];
  /** Lanes in use on this row (before or after). */
  width: number;
}

/**
 * Lay a page of commits out as lanes, one pass, children before parents (the
 * server walks `--date-order`, which guarantees that). Each lane carries the
 * hash it expects next: a commit takes the lane expecting it (or a fresh one
 * for a branch tip), hands the lane to its first parent, and sends every other
 * parent to the lane already waiting for it or to a new one. Two lanes
 * expecting the same hash converge on that commit's row.
 */
export function layoutGraph(commits: GitCommit[]): GraphRow[] {
  const rows: GraphRow[] = [];
  let lanes: (string | null)[] = [];
  let colors: number[] = [];
  let nextColor = 0;
  const firstFree = (arr: (string | null)[]) => {
    const i = arr.indexOf(null);
    return i === -1 ? arr.length : i;
  };
  for (const c of commits) {
    const before = lanes;
    const beforeColors = colors;
    let lane = before.indexOf(c.hash);
    let color: number;
    const into: GraphRow["into"] = [];
    if (lane === -1) {
      lane = firstFree(before);
      color = nextColor++ % LANE_COLORS.length;
    } else {
      color = beforeColors[lane];
      for (let k = 0; k < before.length; k++) {
        if (before[k] === c.hash) into.push({ lane: k, color: beforeColors[k] });
      }
    }
    const after = before.slice();
    const afterColors = beforeColors.slice();
    while (after.length <= lane) {
      after.push(null);
      afterColors.push(0);
    }
    for (let k = 0; k < after.length; k++) {
      if (after[k] === c.hash) after[k] = null;
    }
    const through: GraphRow["through"] = [];
    for (let k = 0; k < before.length; k++) {
      if (before[k] !== null && before[k] !== c.hash && k !== lane) {
        through.push({ lane: k, color: beforeColors[k] });
      }
    }
    const out: GraphRow["out"] = [];
    const [first, ...rest] = c.parents;
    if (first !== undefined) {
      after[lane] = first;
      afterColors[lane] = color;
      out.push({ lane, color });
      for (const p of rest) {
        let j = after.indexOf(p);
        if (j === -1) {
          j = firstFree(after);
          if (j === after.length) {
            after.push(null);
            afterColors.push(0);
          }
          after[j] = p;
          afterColors[j] = nextColor++ % LANE_COLORS.length;
        }
        out.push({ lane: j, color: afterColors[j] });
      }
    }
    while (after.length > 0 && after[after.length - 1] === null) {
      after.pop();
      afterColors.pop();
    }
    rows.push({
      lane,
      color,
      into,
      out,
      through,
      width: Math.max(before.length, after.length, lane + 1),
    });
    lanes = after;
    colors = afterColors;
  }
  return rows;
}

function GraphCell({ row, merge, lanes }: { row: GraphRow; merge: boolean; lanes: number }) {
  const x = (k: number) => k * LANE_W + LANE_W / 2;
  const col = (c: number) => LANE_COLORS[c % LANE_COLORS.length];
  const cx = x(row.lane);
  const cy = ROW_H / 2;
  const w = lanes * LANE_W;
  return (
    <svg className="git-graph" width={w} height={ROW_H} viewBox={`0 0 ${w} ${ROW_H}`} aria-hidden="true">
      {row.through.map((t) => (
        <line key={`t${t.lane}`} x1={x(t.lane)} y1={0} x2={x(t.lane)} y2={ROW_H} stroke={col(t.color)} />
      ))}
      {row.into.map((i) =>
        i.lane === row.lane ? (
          <line key={`i${i.lane}`} x1={cx} y1={0} x2={cx} y2={cy} stroke={col(i.color)} />
        ) : (
          <path
            key={`i${i.lane}`}
            d={`M${x(i.lane)},0 C${x(i.lane)},${cy} ${cx},0 ${cx},${cy}`}
            stroke={col(i.color)}
          />
        ),
      )}
      {row.out.map((o) =>
        o.lane === row.lane ? (
          <line key={`o${o.lane}`} x1={cx} y1={cy} x2={cx} y2={ROW_H} stroke={col(o.color)} />
        ) : (
          <path
            key={`o${o.lane}`}
            d={`M${cx},${cy} C${cx},${ROW_H} ${x(o.lane)},${cy} ${x(o.lane)},${ROW_H}`}
            stroke={col(o.color)}
          />
        ),
      )}
      <circle
        cx={cx}
        cy={cy}
        r={merge ? 3 : 3.5}
        fill={merge ? "var(--panel, #0e121a)" : col(row.color)}
        stroke={col(row.color)}
        strokeWidth={merge ? 1.5 : 0}
      />
    </svg>
  );
}

// ---- small pieces --------------------------------------------------------

function RefChip({ r, onPick }: { r: GitRef; onPick?: (name: string) => void }) {
  const title =
    r.kind === "head"
      ? "detached HEAD"
      : `${r.kind === "tag" ? "tag" : `${r.kind} branch`} ${r.name}${r.current ? " (checked out)" : ""}`;
  const cls = `git-ref ${r.kind}${r.current ? " current" : ""}`;
  if (!onPick || r.kind === "tag" || r.kind === "head") {
    return <span className={cls} title={title}>{r.name}</span>;
  }
  return (
    <button
      type="button"
      className={cls}
      title={`${title} — click to show only this branch`}
      onClick={(e) => {
        e.stopPropagation();
        onPick(r.name);
      }}
    >
      {r.name}
    </button>
  );
}

/** Themed pick-list (native <select> popups ignore the app theme). Same shape
 *  as the branch checkout dropdown: trigger + absolute menu, outside click
 *  closes, filter box once the list is long. */
function PickMenu({
  label,
  value,
  groups,
  onChange,
}: {
  label: string;
  value: string | null;
  groups: { title?: string; options: { value: string | null; label: string }[] }[];
  onChange: (value: string | null) => void;
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

  const total = groups.reduce((n, g) => n + g.options.length, 0);
  const q = query.trim().toLowerCase();
  const autoFocus = !window.matchMedia("(pointer: coarse)").matches;
  const pick = (v: string | null) => {
    setOpen(false);
    setQuery("");
    onChange(v);
  };

  return (
    <div className="git-branch-dd git-pick" ref={ref}>
      <button
        type="button"
        className={`git-branch-trigger git-pick-trigger${value !== null ? " set" : ""}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="git-branch-cur">{label}</span>
        <ChevronIcon size={11} open={open} className="row-chevron" />
      </button>
      {open && (
        <div className="git-branch-menu" role="listbox">
          {total > 8 && (
            <input
              className="git-branch-filter"
              placeholder="Filter…"
              value={query}
              autoFocus={autoFocus}
              spellCheck={false}
              autoCapitalize="off"
              autoCorrect="off"
              onChange={(e) => setQuery(e.target.value)}
            />
          )}
          <div className="git-branch-list">
            {groups.map((g, gi) => {
              const opts = g.options.filter((o) => !q || o.label.toLowerCase().includes(q));
              if (opts.length === 0) return null;
              return (
                <div key={gi}>
                  {g.title && <div className="git-branch-group">{g.title}</div>}
                  {opts.map((o) => (
                    <button
                      key={o.value ?? "\0all"}
                      type="button"
                      role="option"
                      aria-selected={o.value === value}
                      className={`git-branch-opt${o.value === value ? " on" : ""}`}
                      onClick={() => pick(o.value)}
                    >
                      {o.label}
                    </button>
                  ))}
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

function CopyHash({ hash }: { hash: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      className="git-copy"
      title={`Copy ${hash}`}
      onClick={() => {
        void copyText(hash).then((ok) => {
          if (!ok) return;
          setDone(true);
          setTimeout(() => setDone(false), 1200);
        });
      }}
    >
      {done ? <CheckIcon size={12} /> : <CopyIcon size={12} />}
    </button>
  );
}

/** Pane width, so the layout can pick side-by-side or one-at-a-time. A
 *  hidden pane reports 0 and keeps whatever it last knew. */
function usePaneWidth(ref: RefObject<HTMLElement | null>): number {
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => {
      const w = entry.contentRect.width;
      if (w > 0) setWidth(w);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [ref]);
  return width;
}

// ---- commit details --------------------------------------------------------

function CommitDetails({
  details,
  error,
  onBack,
  onOpenFile,
  onPickBranch,
}: {
  details: GitCommitDetails | null;
  error: string | null;
  onBack: (() => void) | null;
  onOpenFile: (f: GitCommitFile) => void;
  onPickBranch: (name: string) => void;
}) {
  const totals = useMemo(() => {
    let add = 0;
    let del = 0;
    for (const f of details?.files ?? []) {
      add += f.additions ?? 0;
      del += f.deletions ?? 0;
    }
    return { add, del };
  }, [details]);

  return (
    <div className="git-log-detail">
      {onBack && (
        <header className="git-head">
          <div className="git-head-top">
            <button type="button" className="git-back" onClick={onBack}>
              <ChevronIcon size={12} className="git-back-chev" /> History
            </button>
            {details && <span className="git-detail-hash">{details.short}</span>}
          </div>
        </header>
      )}
      <div className="git-detail-scroll">
        {error && <div className="git-empty git-error">{error}</div>}
        {!error && !details && <div className="git-empty">Loading…</div>}
        {details && (
          <>
            <h3 className="git-detail-subject">{details.subject}</h3>
            <div className="git-detail-meta">
              <span className="git-detail-hash" title={details.hash}>{details.short}</span>
              <CopyHash hash={details.hash} />
              <span title={details.authorEmail}>{details.author}</span>
              <span className="git-detail-sep">·</span>
              <span title={formatFullDateTime(details.at)}>{formatCompactDateTime(details.at)}</span>
              {details.committer !== details.author && (
                <span className="git-detail-committer" title={formatFullDateTime(details.committedAt)}>
                  committed by {details.committer}
                </span>
              )}
            </div>
            {details.refs.length > 0 && (
              <div className="git-detail-refs">
                {details.refs.map((r) => (
                  <RefChip key={`${r.kind}:${r.name}`} r={r} onPick={onPickBranch} />
                ))}
              </div>
            )}
            {details.body && <pre className="git-detail-body">{details.body}</pre>}
            <section className="git-section">
              <h3 className="git-section-title">
                {details.files.length} file{details.files.length === 1 ? "" : "s"}
                <span className="git-file-counts">
                  {totals.add > 0 && <span className="add">+{totals.add}</span>}
                  {totals.del > 0 && <span className="del">−{totals.del}</span>}
                </span>
              </h3>
              {details.parents.length > 1 && (
                <p className="git-sheet-hint">Merge commit — changes are shown against the first parent.</p>
              )}
              {details.files.length === 0 && <div className="git-empty">No file changes.</div>}
              {details.files.map((f) => (
                <div key={f.path} className="git-row">
                  <button
                    type="button"
                    className="git-row-main"
                    title={f.origPath ? `${f.origPath} → ${f.path}` : f.path}
                    onClick={() => onOpenFile(f)}
                  >
                    <span className={`git-letter s-${f.status}`} title={statusLabel(f.status)}>{f.status}</span>
                    <PathLabel path={f.path} />
                    <span className="git-file-counts">
                      {f.binary && <span>bin</span>}
                      {(f.additions ?? 0) > 0 && <span className="add">+{f.additions}</span>}
                      {(f.deletions ?? 0) > 0 && <span className="del">−{f.deletions}</span>}
                    </span>
                  </button>
                </div>
              ))}
            </section>
          </>
        )}
      </div>
    </div>
  );
}

function CommitFileDiff({
  repoId,
  hash,
  file,
  onBack,
}: {
  repoId: string;
  hash: string;
  file: GitCommitFile;
  onBack: () => void;
}) {
  const { actions } = useStore();
  const diff = useDiff(
    () => actions.gitCommitDiff(repoId, hash, file.path, file.origPath),
    `${repoId}\0${hash}\0${file.path}`,
  );
  return (
    <div className="git-log-detail">
      <header className="git-head">
        <div className="git-head-top">
          <button type="button" className="git-back" onClick={onBack}>
            <ChevronIcon size={12} className="git-back-chev" /> Files
          </button>
          <span className="git-diff-path" title={file.path}>{file.path}</span>
          <span className={`git-letter s-${file.status}`} title={statusLabel(file.status)}>{file.status}</span>
          {diff.meta?.truncated && <span className="git-chip warn">truncated</span>}
        </div>
      </header>
      <div className="git-detail-scroll">
        <DiffBody {...diff} />
      </div>
    </div>
  );
}

// ---- the log ---------------------------------------------------------------

/**
 * Commit history for one repo: a branch graph beside each subject, the refs
 * sitting on it, author and date; select a commit for its message and files,
 * a file for its patch. Wide panes show the list and the commit side by side;
 * narrow ones (the phone) drill in one screen at a time. Filters: message
 * text or a hash, one branch, one author.
 */
export function GitLogView({
  repo,
  branches,
  current,
  refreshTick,
}: {
  repo: GitRepoInfo;
  branches: GitBranchesData | null;
  current: string | undefined;
  refreshTick: number;
}) {
  const { actions } = useStore();
  const rootRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const width = usePaneWidth(rootRef);
  const wide = width >= WIDE_MIN;
  const mid = width >= MID_MIN;

  const [commits, setCommits] = useState<GitCommit[]>([]);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [queryInput, setQueryInput] = useState("");
  const [query, setQuery] = useState("");
  const [branch, setBranch] = useState<string | null>(null);
  const [author, setAuthor] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [details, setDetails] = useState<GitCommitDetails | null>(null);
  const [detailsError, setDetailsError] = useState<string | null>(null);
  const [file, setFile] = useState<GitCommitFile | null>(null);
  /** Bumped per request so a slow page can't land on top of newer filters. */
  const seq = useRef(0);
  const commitsRef = useRef<GitCommit[]>([]);
  commitsRef.current = commits;

  useEffect(() => {
    const t = setTimeout(() => setQuery(queryInput.trim()), 250);
    return () => clearTimeout(t);
  }, [queryInput]);

  const load = useCallback(
    async (reset: boolean) => {
      const my = ++seq.current;
      setLoading(true);
      setError(null);
      try {
        const d = await actions.gitLog(repo.id, {
          limit: PAGE,
          skip: reset ? 0 : commitsRef.current.length,
          ...(branch ? { branch } : {}),
          ...(query ? { query } : {}),
          ...(author ? { author } : {}),
        });
        if (my !== seq.current) return;
        setCommits((prev) => (reset ? d.commits : [...prev, ...d.commits]));
        setHasMore(d.hasMore);
      } catch (e) {
        if (my === seq.current) setError(errText(e));
      } finally {
        if (my === seq.current) setLoading(false);
      }
    },
    [actions, repo.id, branch, query, author],
  );

  // First page on mount and whenever a filter, the head commit (a turn just
  // committed) or the header's Refresh changes.
  useEffect(() => {
    void load(true);
  }, [load, repo.lastCommit?.hash, refreshTick]);

  // Side by side, the details column should never sit empty.
  useEffect(() => {
    if (wide && selected === null && commits.length > 0) setSelected(commits[0].hash);
  }, [wide, selected, commits]);

  useEffect(() => {
    if (!selected) {
      setDetails(null);
      return;
    }
    let gone = false;
    setDetails(null);
    setDetailsError(null);
    setFile(null);
    actions
      .gitShow(repo.id, selected)
      .then((d) => !gone && setDetails(d))
      .catch((e) => !gone && setDetailsError(errText(e)));
    return () => {
      gone = true;
    };
  }, [actions, repo.id, selected]);

  const rows = useMemo(() => layoutGraph(commits), [commits]);
  const lanes = useMemo(
    () => Math.min(MAX_LANES, Math.max(1, ...rows.map((r) => r.width))),
    [rows],
  );
  const authors = useMemo(
    () => Array.from(new Set(commits.map((c) => c.author))).sort((a, b) => a.localeCompare(b)),
    [commits],
  );

  const move = (delta: number) => {
    if (commits.length === 0) return;
    const i = commits.findIndex((c) => c.hash === selected);
    const next = commits[Math.min(commits.length - 1, Math.max(0, (i === -1 ? -1 : i) + delta))];
    setSelected(next.hash);
    listRef.current
      ?.querySelector(`[data-hash="${next.hash}"]`)
      ?.scrollIntoView({ block: "nearest" });
  };

  const onScroll = () => {
    const el = listRef.current;
    if (!el || loading || !hasMore) return;
    if (el.scrollTop + el.clientHeight >= el.scrollHeight - ROW_H * 6) void load(false);
  };

  const branchGroups = [
    { options: [{ value: null, label: "All branches" }] },
    ...(branches
      ? [
          { title: "Local", options: branches.branches.map((b) => ({ value: b, label: b })) },
          {
            title: "Remote",
            options: (branches.remoteBranches ?? []).map((b) => ({ value: b, label: b })),
          },
        ]
      : current
        ? [{ options: [{ value: current, label: current }] }]
        : []),
  ];

  const filtered = branch !== null || author !== null || query.length > 0;
  // Wide: list beside either the commit or the open file's patch; narrow: one
  // of the three at a time.
  const showList = wide || (selected === null && file === null);
  const showDetail = selected !== null && file === null;
  const showDiff = selected !== null && file !== null;

  const list = (
    <div
      className="git-log-list"
      ref={listRef}
      role="listbox"
      aria-label="Commits"
      tabIndex={0}
      style={{ "--graph-w": `${lanes * LANE_W}px` } as CSSProperties}
      onScroll={onScroll}
      onKeyDown={(e) => {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          move(1);
        } else if (e.key === "ArrowUp") {
          e.preventDefault();
          move(-1);
        }
      }}
    >
      {error && <div className="git-empty git-error">{error}</div>}
      {!error && commits.length === 0 && !loading && (
        <div className="git-empty">{filtered ? "No commits match." : "No commits yet."}</div>
      )}
      {commits.map((c, i) => {
        const merge = c.parents.length > 1;
        return (
          <div
            key={c.hash}
            data-hash={c.hash}
            role="option"
            aria-selected={c.hash === selected}
            className={`git-log-row${c.hash === selected ? " on" : ""}${merge ? " merge" : ""}`}
            onClick={() => setSelected(c.hash)}
          >
            <GraphCell row={rows[i]} merge={merge} lanes={lanes} />
            <span className="git-log-subject">
              <span className="git-log-subject-text" title={c.subject}>{c.subject}</span>
              {c.refs.length > 0 && (
                <span className="git-log-refs">
                  {c.refs.map((r) => (
                    <RefChip key={`${r.kind}:${r.name}`} r={r} onPick={setBranch} />
                  ))}
                </span>
              )}
            </span>
            <span className="git-log-author" title={c.authorEmail}>{c.author}</span>
            <span className="git-log-date" title={formatFullDateTime(c.at)}>
              {formatCompactDateTime(c.at)}
            </span>
          </div>
        );
      })}
      {loading && <div className="git-empty">Loading…</div>}
      {!loading && hasMore && (
        <button type="button" className="git-log-more" onClick={() => void load(false)}>
          Load more
        </button>
      )}
    </div>
  );

  return (
    <div className={`git-log${wide ? " wide" : ""}${mid ? " mid" : ""}`} ref={rootRef}>
      {showList && (
        <div className="git-log-bar">
          <label className="git-log-search">
            <SearchIcon size={13} />
            <input
              type="search"
              placeholder="Text or hash"
              value={queryInput}
              spellCheck={false}
              autoCapitalize="off"
              autoCorrect="off"
              onChange={(e) => setQueryInput(e.target.value)}
            />
            {queryInput && (
              <button type="button" className="git-log-clear" aria-label="Clear search" onClick={() => setQueryInput("")}>
                <XIcon size={12} />
              </button>
            )}
          </label>
          <PickMenu
            label={branch ?? "All branches"}
            value={branch}
            groups={branchGroups}
            onChange={setBranch}
          />
          <PickMenu
            label={author ?? "Anyone"}
            value={author}
            groups={[
              { options: [{ value: null, label: "Anyone" }] },
              { options: authors.map((a) => ({ value: a, label: a })) },
            ]}
            onChange={setAuthor}
          />
        </div>
      )}
      <div className="git-log-body">
        {showList && list}
        {showDetail && (
          <CommitDetails
            details={details}
            error={detailsError}
            onBack={wide ? null : () => setSelected(null)}
            onOpenFile={setFile}
            onPickBranch={(name) => {
              setBranch(name);
              if (!wide) setSelected(null);
            }}
          />
        )}
        {showDiff && selected && file && (
          <CommitFileDiff repoId={repo.id} hash={selected} file={file} onBack={() => setFile(null)} />
        )}
      </div>
    </div>
  );
}
