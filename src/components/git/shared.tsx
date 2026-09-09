import { useEffect, useState } from "react";
import type { GitDiffData } from "../../lib/protocol";

/** Porcelain status letter → human label. */
export function statusLabel(c: string): string {
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

export function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Path rendered as dim directory + bright filename so the part that matters
 *  stays readable; the directory truncates first when space runs out. */
export function PathLabel({ path }: { path: string }) {
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

export interface DiffState {
  unified: string | null;
  meta: { truncated: boolean; binary: boolean } | null;
  error: string | null;
}

/** Fetch a diff; refetches whenever `key` changes (the caller folds every
 *  input into it, so `load` itself can be an inline closure). */
export function useDiff(load: () => Promise<GitDiffData>, key: string): DiffState {
  const [state, setState] = useState<DiffState>({ unified: null, meta: null, error: null });
  useEffect(() => {
    let gone = false;
    setState({ unified: null, meta: null, error: null });
    load()
      .then((d) => {
        if (gone) return;
        setState({ unified: d.unified, meta: { truncated: d.truncated, binary: d.binary }, error: null });
      })
      .catch((e) => !gone && setState({ unified: null, meta: null, error: errText(e) }));
    return () => {
      gone = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  return state;
}

/** The unified-diff body shared by the worktree and history viewers. */
export function DiffBody({ unified, meta, error }: DiffState) {
  if (error) return <div className="git-empty git-error">{error}</div>;
  if (unified === null) return <div className="git-empty">Loading…</div>;
  if (meta?.binary) return <div className="git-empty">Binary file.</div>;
  if (unified.length === 0) return <div className="git-empty">No changes.</div>;
  return (
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
  );
}
