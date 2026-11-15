import type { ModelOption } from "@/bindings/ModelOption";
import type { Provider } from "@/bindings/Provider";
import catalog from "../src-tauri/model-catalog/v1.json";

export const exampleSettingsModels: ModelOption[] = catalog.models.map((model) => ({
  selection: { provider: model.provider as Provider, profileId: "simulation-settings", modelId: model.id },
  name: model.name,
  catalogVersion: catalog.version,
  rating: {
    intelligenceTenths: model.intelligenceTenths,
    apiReferenceCents: model.apiReferenceCents,
    priceScore: model.priceScore,
  },
  selectable: true,
  unavailableReason: null,
}));
