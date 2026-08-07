import type { GitRepoInfo, Project } from "./protocol";

/**
 * Which repo owns `path` (longest-relPath-prefix match). Paths from agent
 * events may be absolute — they're normalized against the project root first.
 * Returns null for single-repo projects (a badge would be noise) or when the
 * path falls outside every repo.
 */
export function repoForPath(
  repos: GitRepoInfo[] | undefined,
  project: Project | undefined,
  path: string,
): GitRepoInfo | null {
  if (!repos || repos.length < 2) return null;
  let rel = path;
  if (project && path.startsWith(project.path)) {
    rel = path.slice(project.path.length).replace(/^\/+/, "");
  }
  let best: GitRepoInfo | null = null;
  for (const r of repos) {
    if (!r.relPath) continue; // "" (root repo) only occurs in single-repo projects
    if (rel === r.relPath || rel.startsWith(r.relPath + "/")) {
      if (!best || r.relPath.length > best.relPath.length) best = r;
    }
  }
  return best;
}
