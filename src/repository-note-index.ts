import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { useDeferredValue } from "react";
import type { KnowledgeSearchHit } from "@/knowledge-navigation";

export type IndexedNote = {
  id: number;
  path: string;
  title: string;
  tags: string[];
  frontmatter: string | null;
};

export function useRepositoryNoteIndex(checkoutId: string, enabled: boolean, query: string) {
  const searchTerm = useDeferredValue(query.trim());
  const index = useQuery({
    queryKey: ["repository-note-index", checkoutId],
    enabled,
    refetchOnWindowFocus: false,
    queryFn: () => invoke<IndexedNote[]>("rebuild_repository_note_index", { checkoutId }),
  });
  const search = useQuery({
    queryKey: ["repository-note-search", checkoutId, searchTerm, index.dataUpdatedAt],
    enabled: enabled && index.isSuccess && Boolean(searchTerm),
    queryFn: () => invoke<KnowledgeSearchHit[]>("search_repository_note_index", { checkoutId, query: searchTerm, limit: 100 }),
  });
  return {
    index,
    hits: searchTerm === query.trim() ? search.data ?? [] : [],
    searching: Boolean(query.trim()) && (index.isPending || search.isFetching || searchTerm !== query.trim()),
    error: index.error ?? search.error,
  };
}
