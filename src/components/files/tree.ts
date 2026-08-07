import type { TreeEntry } from "../../lib/protocol";

/** A node in the client-built collapsible tree. `name` may contain "/" after
 *  single-child directory chains are flattened. */
export interface TreeNode {
  name: string;
  /** Project-root-relative POSIX path (no trailing slash). */
  path: string;
  kind: "file" | "dir";
  sizeBytes?: number;
  children?: TreeNode[];
}

function parentOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? "" : path.slice(0, i);
}

function baseOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? path : path.slice(i + 1);
}

/** Build a nested tree from the flat `fs.tree` entry list. Directories come
 *  before files, each level sorted case-insensitively. Single-child directory
 *  chains (a dir whose sole child is another dir) are collapsed into one row. */
export function buildTree(entries: TreeEntry[]): TreeNode[] {
  const rootChildren: TreeNode[] = [];
  const dirs = new Map<string, TreeNode>();

  const ensureDir = (path: string): TreeNode => {
    const existing = dirs.get(path);
    if (existing) return existing;
    const node: TreeNode = { name: baseOf(path), path, kind: "dir", children: [] };
    dirs.set(path, node);
    const parent = parentOf(path);
    const siblings = parent ? ensureDir(parent).children! : rootChildren;
    siblings.push(node);
    return node;
  };

  for (const entry of entries) {
    if (entry.kind === "dir") {
      ensureDir(entry.path);
    } else {
      const parent = parentOf(entry.path);
      const siblings = parent ? ensureDir(parent).children! : rootChildren;
      siblings.push({
        name: baseOf(entry.path),
        path: entry.path,
        kind: "file",
        sizeBytes: entry.sizeBytes,
      });
    }
  }

  sortLevel(rootChildren);
  return rootChildren.map(flattenChains);
}

function sortLevel(nodes: TreeNode[]): void {
  nodes.sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === "dir" ? -1 : 1;
    return a.name.toLowerCase().localeCompare(b.name.toLowerCase());
  });
  for (const n of nodes) if (n.children) sortLevel(n.children);
}

/** Collapse `a → b → c` chains where each dir holds a single sub-directory. */
function flattenChains(node: TreeNode): TreeNode {
  if (node.kind !== "dir" || !node.children) return node;
  let cur = node;
  while (cur.children && cur.children.length === 1 && cur.children[0].kind === "dir") {
    const only = cur.children[0];
    cur = { ...only, name: `${node.name === cur.name ? node.name : cur.name}/${only.name}` };
  }
  // Rebuild the merged label across the whole collapsed chain.
  if (cur !== node) {
    const label = mergedLabel(node);
    return { ...cur, name: label, children: cur.children?.map(flattenChains) };
  }
  return { ...node, children: node.children.map(flattenChains) };
}

/** Walk a single-child dir chain to produce the "a/b/c" label. */
function mergedLabel(node: TreeNode): string {
  const parts = [node.name];
  let cur = node;
  while (cur.children && cur.children.length === 1 && cur.children[0].kind === "dir") {
    cur = cur.children[0];
    parts.push(cur.name);
  }
  return parts.join("/");
}

export interface FilterResult {
  /** Paths to render: matches (up to `cap`) plus their ancestor dirs. */
  visible: Set<string>;
  /** Total matches in the tree (files + dirs whose path matches). */
  total: number;
  /** Matches actually included (≤ cap). */
  shown: number;
}

/**
 * One O(n) pass over the tree: collect up to `cap` matching nodes (case-
 * insensitive substring on the full relative path, tree order) and every
 * ancestor dir needed to show them. Rendering then only consults the Set —
 * no per-row subtree walks, and a broad query can't explode the DOM.
 */
export function filterTree(tree: TreeNode[], q: string, cap: number): FilterResult {
  const visible = new Set<string>();
  let total = 0;
  let shown = 0;
  const stack: string[] = [];

  const admit = (path: string) => {
    total++;
    if (shown >= cap) return;
    shown++;
    visible.add(path);
    for (const a of stack) visible.add(a);
  };

  const walk = (node: TreeNode) => {
    if (node.path.toLowerCase().includes(q)) admit(node.path);
    if (node.kind === "dir" && node.children) {
      stack.push(node.path);
      for (const c of node.children) walk(c);
      stack.pop();
    }
  };

  for (const n of tree) walk(n);
  return { visible, total, shown };
}
