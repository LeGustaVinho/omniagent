import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { RefreshCw } from "lucide-react";
import type { CatalogRefreshStatus } from "@/bindings/CatalogRefreshStatus";
import { Button } from "@/components/ui/button";
import { catalogStatusKey, invalidateModelCatalog, refreshCatalogKey } from "@/model-catalog-query";
import { SettingRow } from "@/settings-controls";

export const modelCatalogSetting = {
  id: "model-catalog",
  label: "Model catalog",
  description: "Refresh API reference prices and intelligence benchmarks for the models available in your CLIs.",
};

export function ModelCatalogSettings() {
  const client = useQueryClient();
  const pending = useIsMutating({ mutationKey: refreshCatalogKey }) > 0;
  const status = useQuery({
    queryKey: catalogStatusKey,
    queryFn: () => invoke<CatalogRefreshStatus>("model_catalog_status"),
    refetchInterval: (query) => query.state.data?.running ? 2000 : false,
  });
  const update = useMutation({
    mutationKey: refreshCatalogKey,
    mutationFn: () => invoke<CatalogRefreshStatus>("refresh_model_catalog"),
    onSuccess: (result) => {
      const previous = client.getQueryData<CatalogRefreshStatus>(catalogStatusKey);
      client.setQueryData(catalogStatusKey, result);
      if (previous?.revision !== result.revision) void invalidateModelCatalog(client);
    },
  });
  const running = pending || status.data?.running;
  const action = running ? "Updating model catalog…" : "Update model catalog";
  const updatedAt = status.data?.updatedAt;
  const issues = status.data?.issues ?? [];
  const incomplete = status.data?.pending ?? [];
  return <SettingRow {...modelCatalogSetting}>
    <div className="grid justify-items-start gap-2">
      <Button id={modelCatalogSetting.id} type="button" variant="outline" disabled={running}
        aria-label={action} aria-describedby="model-catalog-description" onClick={() => update.mutate()}>
        <RefreshCw size={14} aria-hidden />
        {action}
      </Button>
      <div role="status" className="text-xs text-muted-foreground">
        {running ? "Checking CLI inventories and researching published prices and benchmarks."
          : updatedAt != null
            ? `${status.data?.modelCount ?? 0} models · Updated ${new Date(updatedAt * 1000).toLocaleString()}`
            : "Using the bundled catalog."}
      </div>
      {(update.error || status.error) && <p role="alert" className="settings-result settings-result-error">
        {String(update.error ?? status.error)}
      </p>}
      {issues.length > 0 && <ul className="grid gap-1 text-xs text-muted-foreground">
        {issues.map((issue, index) => <li key={index}>{issue}</li>)}
      </ul>}
      {incomplete.length > 0 && <details className="text-xs text-muted-foreground">
        <summary className="cursor-pointer">{incomplete.length} {incomplete.length === 1 ? "model" : "models"} with pending or earlier data</summary>
        <ul className="mt-2 grid gap-1">
          {incomplete.map((model) => <li key={model.provider + "/" + model.id}>
            {model.name} ({model.provider}) · {model.sources}/4 benchmarks
            {!model.price && " · Price pending"}{!model.intelligence && " · Intelligence pending"}
            {model.reused && " · Earlier data used"}
          </li>)}
        </ul>
      </details>}
    </div>
  </SettingRow>;
}
