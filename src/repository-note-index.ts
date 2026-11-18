import { Channel, invoke } from "@tauri-apps/api/core";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useDeferredValue, useEffect, useState } from "react";
import type { KnowledgeSearchHit } from "@/knowledge-navigation";

export type IndexedNote = {
  id: number;
  path: string;
  title: string;
  tags: string[];
  frontmatter: string | null;
  preview?: string | null;
};

type IndexInvalidation = { checkoutId: string } & ({ kind: "paths"; paths: string[] } | { kind: "rebuild"; reason: string });

const indexKey = (checkoutId: string) => ["repository-note-index", checkoutId];

export function useRepositoryNoteIndex(checkoutId: string, query: string) {
  const client = useQueryClient();
  const [watching, setWatching] = useState(false);
  const [watchError, setWatchError] = useState("");
  useEffect(() => {
    let active = true;
    let watchId: string | undefined;
    let pending = Promise.resolve();
    const unwatch = (id: string) => invoke("unwatch_repository_note_index", { watchId: id }).catch(() => undefined);
    const channel = new Channel<IndexInvalidation>();
    channel.onmessage = (notice) => {
      if (!active || notice.checkoutId !== checkoutId) return;
      pending = pending.then(async () => {
        if (!active) return;
        const paths = notice.kind === "paths" ? notice.paths : null;
        const notes = await invoke<IndexedNote[]>("reconcile_repository_note_index", { checkoutId, paths });
        if (active) {
          client.setQueryData(indexKey(checkoutId), notes);
          setWatchError("");
        }
      }).catch((failure: unknown) => { if (active) setWatchError(String(failure)); });
    };
    void invoke<string>("watch_repository_note_index", { checkoutId, channel }).then((id) => {
      if (!active) return unwatch(id);
      watchId = id;
      setWatching(true);
    }).catch((failure: unknown) => {
      if (!active) return;
      setWatchError(String(failure));
      setWatching(true);
    });
    return () => {
      active = false;
      if (watchId) void unwatch(watchId);
    };
  }, [checkoutId, client]);

  const searchTerm = useDeferredValue(query.trim());
  const index = useQuery({
    queryKey: indexKey(checkoutId),
    enabled: watching,
    refetchOnWindowFocus: false,
    queryFn: () => invoke<IndexedNote[]>("rebuild_repository_note_index", { checkoutId }),
  });
  const search = useQuery({
    queryKey: ["repository-note-search", checkoutId, searchTerm, index.dataUpdatedAt],
    enabled: index.isSuccess && Boolean(searchTerm),
    queryFn: () => invoke<KnowledgeSearchHit[]>("search_repository_note_index", { checkoutId, query: searchTerm, limit: 100 }),
  });
  return {
    index,
    hits: searchTerm === query.trim() ? search.data ?? [] : [],
    searching: Boolean(query.trim()) && (index.isPending || search.isFetching || searchTerm !== query.trim()),
    error: index.error ?? search.error,
    watchError,
  };
}
