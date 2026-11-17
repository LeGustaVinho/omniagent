import { useCallback, useMemo, useSyncExternalStore } from "react";

export type CheckoutPathFavorite = { kind: "file" | "folder"; path: string; ignored?: boolean; groupId?: string };
export type CheckoutNoteFavorite = { kind: "heading" | "block"; path: string; anchor: string; label: string; occurrence: number; groupId?: string };
export type CheckoutFavorite = CheckoutPathFavorite | CheckoutNoteFavorite;
type FavoriteTarget = CheckoutFavorite | string;
export type CheckoutFavoriteGroup = { id: string; name: string };
type CheckoutFavorites = { favorites: readonly CheckoutFavorite[]; groups: readonly CheckoutFavoriteGroup[] };

const favoritesChangedEvent = "omniagent:checkout-favorites-changed";
const inMemory = new Map<string, string>();
const storageKey = (checkoutId: string) => `omniagent:checkout-favorites:${encodeURIComponent(checkoutId)}`;
export const isNoteFavorite = (item: CheckoutFavorite): item is CheckoutNoteFavorite => item.kind === "heading" || item.kind === "block";

export function checkoutFavoriteKey(item: FavoriteTarget) {
  const path = (typeof item === "string" ? item : item.path).toLowerCase();
  return typeof item !== "string" && isNoteFavorite(item)
    ? JSON.stringify([item.kind, path, item.anchor, item.occurrence])
    : JSON.stringify(["path", path]);
}
const emptyFavorites: CheckoutFavorites = { favorites: [], groups: [] };
export const maxFavoriteGroupName = 80;

function snapshot(key: string): string {
  const fallback = inMemory.get(key);
  if (fallback !== undefined) return fallback;
  try { return localStorage.getItem(key) ?? "[]"; }
  catch { return "[]"; }
}

function parse(value: string): CheckoutFavorites {
  try {
    const parsed: unknown = JSON.parse(value);
    const stored = parsed as Partial<CheckoutFavorites> | null;
    const records = Array.isArray(parsed) ? parsed : stored?.favorites;
    const groups = Array.isArray(stored?.groups) ? stored.groups.filter((group): group is CheckoutFavoriteGroup =>
      group != null && typeof group.id === "string" && !!group.id && typeof group.name === "string" && !!group.name.trim()) : [];
    const groupIds = new Set(groups.map(({ id }) => id));
    const favorites: CheckoutFavorite[] = [];
    for (const item of Array.isArray(records) ? records : []) {
      if (item == null || typeof item.path !== "string" || !item.path) continue;
      const groupId = item.groupId && groupIds.has(item.groupId) ? item.groupId : undefined;
      if (item.kind === "file" || item.kind === "folder") {
        favorites.push({ path: item.path, kind: item.kind, ignored: item.ignored === true || undefined, groupId });
      } else if ((item.kind === "heading" || item.kind === "block") && typeof item.anchor === "string" && item.anchor
        && typeof item.label === "string" && item.label.trim() && Number.isSafeInteger(item.occurrence) && item.occurrence >= 0) {
        favorites.push({ kind: item.kind, path: item.path, anchor: item.anchor, label: item.label, occurrence: item.occurrence, groupId });
      }
    }
    return { favorites: [...new Map(favorites.map((item) => [checkoutFavoriteKey(item), item])).values()], groups };
  } catch { /* Start with no favorites if the stored list is unavailable. */ }
  return emptyFavorites;
}

function subscribe(key: string, changed: () => void) {
  const local = (event: Event) => { if ((event as CustomEvent<string>).detail === key) changed(); };
  const other = (event: StorageEvent) => { if (event.key === key || event.key === null) changed(); };
  window.addEventListener(favoritesChangedEvent, local);
  window.addEventListener("storage", other);
  return () => {
    window.removeEventListener(favoritesChangedEvent, local);
    window.removeEventListener("storage", other);
  };
}

export function useCheckoutFavorites(checkoutId: string) {
  const key = storageKey(checkoutId);
  const value = useSyncExternalStore(useCallback((changed) => subscribe(key, changed), [key]), () => snapshot(key));
  return useMemo(() => parse(value), [value]);
}

function updateFavorites(checkoutId: string, change: (current: CheckoutFavorites) => CheckoutFavorites) {
  const key = storageKey(checkoutId);
  const current = snapshot(key);
  const next = JSON.stringify(change(parse(current)));
  if (current === next) return;
  try {
    localStorage.setItem(key, next);
    inMemory.delete(key);
  } catch { inMemory.set(key, next); }
  window.dispatchEvent(new CustomEvent(favoritesChangedEvent, { detail: key }));
}

export function setCheckoutFavorites(checkoutId: string, items: readonly CheckoutFavorite[], remove: boolean) {
  updateFavorites(checkoutId, (current) => {
    const favorites = new Map(current.favorites.map((item) => [checkoutFavoriteKey(item), item]));
    for (const item of items) {
      const key = checkoutFavoriteKey(item);
      if (remove) favorites.delete(key);
      else favorites.set(key, { ...item, groupId: favorites.get(key)?.groupId });
    }
    return { ...current, favorites: [...favorites.values()] };
  });
}

export function reconcileFavoriteItems(checkoutId: string, transform: (path: string) => string | null | undefined) {
  updateFavorites(checkoutId, (current) => {
    const favorites = new Map<string, CheckoutFavorite>();
    const moved = new Set<string>();
    for (const item of current.favorites) {
      const destination = transform(item.path);
      if (destination === null) continue;
      const path = destination ?? item.path;
      const movedItem = { ...item, path };
      const key = checkoutFavoriteKey(movedItem);
      const changed = destination !== undefined && destination !== item.path;
      if (changed || !moved.has(key)) favorites.set(key, movedItem);
      if (changed) moved.add(key);
    }
    return { ...current, favorites: [...favorites.values()] };
  });
}

function assignGroup(favorites: readonly CheckoutFavorite[], targets: readonly FavoriteTarget[], groupId?: string) {
  const matched = new Set(targets.map(checkoutFavoriteKey));
  return favorites.map((item) => matched.has(checkoutFavoriteKey(item)) ? { ...item, groupId } : item);
}

export function moveCheckoutFavorites(checkoutId: string, targets: readonly FavoriteTarget[], groupId?: string) {
  updateFavorites(checkoutId, (current) => {
    if (groupId && !current.groups.some(({ id }) => id === groupId)) return current;
    return { ...current, favorites: assignGroup(current.favorites, targets, groupId) };
  });
}

export function saveCheckoutFavoriteGroup(checkoutId: string, { id, name, paths = [], items = [] }: {
  id?: string; name: string; paths?: readonly string[]; items?: readonly CheckoutFavorite[];
}) {
  const title = name.trim();
  if (!title || title.length > maxFavoriteGroupName) throw new Error("Enter a favorite group name between 1 and 80 characters.");
  updateFavorites(checkoutId, (current) => {
    if (current.groups.some((group) => group.id !== id && group.name.toLowerCase() === title.toLowerCase())) {
      throw new Error("A favorite group with this name already exists.");
    }
    if (id) {
      if (!current.groups.some((group) => group.id === id)) throw new Error("This favorite group is no longer available.");
      return { ...current, groups: current.groups.map((group) => group.id === id ? { id, name: title } : group) };
    }
    const group = { id: crypto.randomUUID(), name: title };
    return { groups: [...current.groups, group], favorites: assignGroup(current.favorites, [...paths, ...items], group.id) };
  });
}
