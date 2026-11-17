import { invoke } from "@tauri-apps/api/core";
import { queryOptions, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { fileName } from "@/ide-knowledge-data";

type Sort = "ascending" | "descending" | "type";
export type TreePreferences = { sort: Sort; dotfiles: boolean; windowsHidden: boolean; ignored: boolean; autoReveal: boolean };
const defaults: TreePreferences = { sort: "ascending", dotfiles: true, windowsHidden: true, ignored: false, autoReveal: false };
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

export function readTreePreferences(key: string): TreePreferences {
  try {
    const saved: Partial<TreePreferences> = JSON.parse(localStorage.getItem(key) ?? "{}");
    return {
      sort: saved.sort === "descending" || saved.sort === "type" ? saved.sort : defaults.sort,
      dotfiles: saved.dotfiles !== false,
      windowsHidden: saved.windowsHidden !== false,
      ignored: saved.ignored === true,
      autoReveal: saved.autoReveal === true,
    };
  } catch { return defaults; }
}

export function saveTreePreference(key: string, value: unknown) {
  try { localStorage.setItem(key, JSON.stringify(value)); }
  catch { /* Keep the in-memory state when storage is unavailable. */ }
}

export function treeParents(path: string) {
  const parts = path.split("/");
  return parts.slice(0, -1).map((_, index) => parts.slice(0, index + 1).join("/"));
}

export type TreeReveal = { path: string; expand?: boolean; includeIgnored?: boolean };

function revealIncludes(path: string, reveal?: TreeReveal) {
  if (!reveal) return false;
  const target = reveal.path.toLowerCase();
  const candidate = path.toLowerCase();
  return candidate === target || target.startsWith(`${candidate}/`) || Boolean(reveal.expand && candidate.startsWith(`${target}/`));
}

export function showTreePath(path: string, dotfiles: boolean, reveals: readonly TreeReveal[] = [], windowsHidden?: ReadonlySet<string>) {
  if (dotfiles && !windowsHidden?.size) return true;
  const targets = reveals.filter((reveal) => revealIncludes(path, reveal)).map(({ path }) => path.toLowerCase());
  return [...treeParents(path), path].every((part) => {
    const hidden = (!dotfiles && fileName(part).startsWith(".")) || windowsHidden?.has(part.toLowerCase());
    if (!hidden) return true;
    const candidate = part.toLowerCase();
    return targets.some((target) => target === candidate || target.startsWith(`${candidate}/`));
  });
}

export interface FolderNodeData {
  folders: Map<string, FolderNodeData>;
  files: string[];
}

function fileType(path: string) {
  const name = fileName(path);
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1) : "";
}

export function makeTree(paths: readonly string[], folders: readonly string[], sort: Sort): FolderNodeData {
  const root: FolderNodeData = { folders: new Map(), files: [] };
  const folderAt = (parts: string[]) => {
    let node = root;
    for (const name of parts) {
      const folder = node.folders.get(name) ?? { folders: new Map(), files: [] };
      node.folders.set(name, folder);
      node = folder;
    }
    return node;
  };
  for (const folder of new Set(folders)) folderAt(folder.split("/"));
  for (const path of new Set(paths)) folderAt(path.split("/").slice(0, -1)).files.push(path);
  const direction = sort === "descending" ? -1 : 1;
  const byName = (left: string, right: string) => direction * collator.compare(left, right);
  function arrange(node: FolderNodeData) {
    node.folders = new Map([...node.folders].sort(([left], [right]) => byName(left, right)));
    node.files.sort((left, right) => (sort === "type" ? collator.compare(fileType(left), fileType(right)) : 0) || byName(fileName(left), fileName(right)));
    for (const folder of node.folders.values()) arrange(folder);
  }
  arrange(root);
  return root;
}

type IgnoredItems = { files: string[]; folders: string[] };
export const ignoredTreeKey = (checkoutId: string) => ["checkout-ignored-tree", checkoutId] as const;
function ignoredFolderQuery(checkoutId: string, folder: string | null) {
  return queryOptions({
    queryKey: [...ignoredTreeKey(checkoutId), folder],
    queryFn: () => invoke<IgnoredItems>("list_checkout_ignored_items", { checkoutId, folder }),
    staleTime: Infinity,
    retry: false,
  });
}

export function useIgnoredTree(checkoutId: string, enabled: boolean, expanded: ReadonlySet<string>, dotfiles: boolean, reveals: readonly TreeReveal[], scope?: string) {
  const root = useQuery({ ...ignoredFolderQuery(checkoutId, null), enabled });
  const rootFolders = root.data?.folders ?? [];
  const inScope = (path: string) => !scope || path === scope || path.startsWith(`${scope}/`) || scope.startsWith(`${path}/`);
  const underIgnoredRoot = (path: string) => rootFolders.some((root) => path === root || path.startsWith(`${root}/`));
  const openIgnoredFolders = enabled ? [...expanded].filter((path) => inScope(path) && underIgnoredRoot(path)
    && showTreePath(path, dotfiles, reveals) && treeParents(path).every((parent) => expanded.has(parent))) : [];
  const children = useQueries({ queries: openIgnoredFolders.map((path) => ignoredFolderQuery(checkoutId, path)) });
  if (!enabled) return { files: [], folders: [], pending: false, error: null, load: new Map<string, { pending: boolean; error: Error | null }>() };
  const load = new Map(openIgnoredFolders.map((path, index) => [path, { pending: children[index]!.isPending, error: children[index]!.error }]));
  return {
    files: [...(root.data?.files ?? []), ...children.flatMap(({ data }) => data?.files ?? [])].filter(inScope),
    folders: [...rootFolders, ...children.flatMap(({ data }) => data?.folders ?? [])].filter(inScope),
    pending: root.isPending, error: root.error, load,
  };
}

export const windowsHiddenTreeKey = (checkoutId: string) => ["checkout-windows-hidden-tree", checkoutId] as const;
function windowsHiddenFolderQuery(checkoutId: string, folder: string) {
  return queryOptions({
    queryKey: [...windowsHiddenTreeKey(checkoutId), folder],
    queryFn: () => invoke<string[]>("list_checkout_windows_hidden_items", { checkoutId, folder }),
    staleTime: Infinity,
    retry: false,
  });
}

export function useWindowsHiddenTree({ checkoutId, enabled, expanded, files, folders, dotfiles, reveals }: {
  checkoutId: string;
  enabled: boolean;
  expanded: ReadonlySet<string>;
  files: readonly string[];
  folders: readonly string[];
  dotfiles: boolean;
  reveals: readonly TreeReveal[];
}) {
  const client = useQueryClient();
  const root = useQuery({ ...windowsHiddenFolderQuery(checkoutId, ""), enabled });
  const openFolders: string[] = [];
  if (enabled && !root.isPending && expanded.size > 0) {
    const known = new Set([...folders, ...[...folders, ...files].flatMap(treeParents)]);
    for (const path of expanded) {
      if (!known.has(path) || !showTreePath(path, dotfiles, reveals)) continue;
      let parent = "";
      const visible = [...treeParents(path), path].every((part) => {
        if (!expanded.has(part)) return false;
        const state = client.getQueryState<string[]>([...windowsHiddenTreeKey(checkoutId), parent]);
        parent = part;
        const name = part.toLowerCase();
        if (!state || state.status === "pending") return false;
        return !state.data?.some((entry) => entry.toLowerCase() === name) || showTreePath(part, true, reveals, new Set([name]));
      });
      if (visible) openFolders.push(path);
    }
  }
  const children = useQueries({ queries: openFolders.map((path) => windowsHiddenFolderQuery(checkoutId, path)) });
  const hidden = enabled ? [...(root.data ?? []), ...children.flatMap(({ data }) => data ?? [])] : [];
  const load = new Map(openFolders.map((path, index) => [path, { pending: children[index]!.isPending, error: children[index]!.error }]));
  return { paths: new Set(hidden.map((path) => path.toLowerCase())), load, pending: enabled && root.isPending, error: enabled ? root.error : null };
}
