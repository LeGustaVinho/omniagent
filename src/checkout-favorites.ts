import { useCallback, useMemo, useSyncExternalStore } from "react";

export type CheckoutFavorite = { kind: "file" | "folder"; path: string; ignored?: boolean };

const favoritesChangedEvent = "omniagent:checkout-favorites-changed";
const inMemory = new Map<string, string>();
const storageKey = (checkoutId: string) => `omniagent:checkout-favorites:${encodeURIComponent(checkoutId)}`;

function snapshot(key: string): string {
  const fallback = inMemory.get(key);
  if (fallback !== undefined) return fallback;
  try { return localStorage.getItem(key) ?? "[]"; }
  catch { return "[]"; }
}

function parse(value: string): CheckoutFavorite[] {
  try {
    const parsed: unknown = JSON.parse(value);
    if (Array.isArray(parsed)) return parsed.filter((item): item is CheckoutFavorite =>
      item != null && (item.kind === "file" || item.kind === "folder") && typeof item.path === "string" && item.path.length > 0);
  } catch { /* Start with no favorites if the stored list is unavailable. */ }
  return [];
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

function updateFavorites(checkoutId: string, change: (list: readonly CheckoutFavorite[]) => readonly CheckoutFavorite[]) {
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

function deduplicate(items: readonly CheckoutFavorite[]) {
  return [...new Map(items.map((item) => [item.path.toLowerCase(), item])).values()];
}

export function setCheckoutFavorites(checkoutId: string, items: readonly CheckoutFavorite[], remove: boolean) {
  const paths = new Set(items.map(({ path }) => path.toLowerCase()));
  updateFavorites(checkoutId, (list) => remove ? list.filter(({ path }) => !paths.has(path.toLowerCase())) : deduplicate([...list, ...items]));
}

export function reconcileFavoriteItems(checkoutId: string, transform: (path: string) => string | null | undefined) {
  updateFavorites(checkoutId, (list) => deduplicate(list.flatMap((item) => {
    const path = transform(item.path);
    return path === null ? [] : [{ ...item, path: path ?? item.path }];
  })));
}
