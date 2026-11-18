import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect } from "react";

export const catalogStatusKey = ["model-catalog-status"];
export const refreshCatalogKey = ["refresh-model-catalog"];

export function invalidateModelCatalog(client: QueryClient) {
  return Promise.all(
    ["conversation-models", "provider-model-inventory", "catalog-model-ratings"].map((name) =>
      client.invalidateQueries({ queryKey: [name] })),
  );
}

export function useModelCatalogEvents() {
  const client = useQueryClient();
  useEffect(() => {
    if (!isTauri()) return;
    const subscription = listen("model-catalog-updated", () => {
      if (client.isMutating({ mutationKey: refreshCatalogKey })) return;
      void client.invalidateQueries({ queryKey: catalogStatusKey });
      void invalidateModelCatalog(client);
    }).catch(() => undefined);
    return () => { void subscription.then((unlisten) => unlisten?.()); };
  }, [client]);
}
