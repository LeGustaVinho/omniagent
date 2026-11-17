import { invoke } from "@tauri-apps/api/core";
import { queryOptions, useQueries, useQuery } from "@tanstack/react-query";
import { fileName } from "@/ide-knowledge-data";

type Sort = "ascending" | "descending" | "type";
export type TreePreferences = { sort: Sort; dotfiles: boolean; ignored: boolean; autoReveal: boolean };
const defaults: TreePreferences = { sort: "ascending", dotfiles: true, ignored: false, autoReveal: false };
const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

export function readTreePreferences(key: string): TreePreferences {
  try {
    const saved: Partial<TreePreferences> = JSON.parse(localStorage.getItem(key) ?? "{}");
    return {
      sort: saved.sort === "descending" || saved.sort === "type" ? saved.sort : defaults.sort,
      dotfiles: saved.dotfiles !== false,
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

export function showTreePath(path: string, dotfiles: boolean, reveal?: string) {
  return dotfiles || path === reveal || reveal?.startsWith(`${path}/`) || !path.split("/").some((part) => part.startsWith("."));
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

export function useIgnoredTree(checkoutId: string, enabled: boolean, expanded: ReadonlySet<string>, dotfiles: boolean, reveal?: string) {
  const root = useQuery({ ...ignoredFolderQuery(checkoutId, null), enabled });
  const rootFolders = root.data?.folders ?? [];
  const underIgnoredRoot = (path: string) => rootFolders.some((root) => path === root || path.startsWith(`${root}/`));
  const openIgnoredFolders = enabled ? [...expanded].filter((path) => underIgnoredRoot(path)
    && showTreePath(path, dotfiles, reveal) && treeParents(path).every((parent) => expanded.has(parent))) : [];
  const children = useQueries({ queries: openIgnoredFolders.map((path) => ignoredFolderQuery(checkoutId, path)) });
  if (!enabled) return { files: [], folders: [], pending: false, error: null, load: new Map<string, { pending: boolean; error: Error | null }>() };
  const load = new Map(openIgnoredFolders.map((path, index) => [path, { pending: children[index]!.isPending, error: children[index]!.error }]));
  return {
    files: [...(root.data?.files ?? []), ...children.flatMap(({ data }) => data?.files ?? [])],
    folders: [...rootFolders, ...children.flatMap(({ data }) => data?.folders ?? [])],
    pending: root.isPending, error: root.error, load,
  };
}
