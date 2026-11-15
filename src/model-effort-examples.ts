import type { ModelSelection } from "@/bindings/ModelSelection";
import type { EffortSet } from "@/components/effort-select";

const throughHigh = ["low", "medium", "high"];
const throughExtraHigh = [...throughHigh, "xhigh"];
const throughMax = [...throughExtraHigh, "max"];
const throughUltra = [...throughMax, "ultra"];

// Snapshot of the local CLI inventories for the models used in the example catalog.
const examples: Record<ModelSelection["provider"], Record<string, EffortSet>> = {
  codex: {
    "gpt-6-astra": { efforts: throughUltra, defaultEffort: "medium" },
    "gpt-6.1-sol": { efforts: throughUltra, defaultEffort: "low" },
    "gpt-6.1-terra": { efforts: throughUltra, defaultEffort: "low" },
    "gpt-5.6-sol": { efforts: throughUltra, defaultEffort: "low" },
    "gpt-6-sol": { efforts: throughUltra, defaultEffort: "medium" },
    "gpt-6-luna": { efforts: throughMax, defaultEffort: "medium" },
  },
  claude: {
    "claude-fable-5-2": { efforts: throughMax },
    "claude-opus-5-5": { efforts: throughMax },
    "claude-sonnet-5-5": { efforts: throughMax },
    "claude-haiku-5-5": { efforts: throughMax },
  },
  antigravity: {
    "gemini-4-argon": { efforts: throughHigh, defaultEffort: "medium" },
    "gemini-4-hydrogen": { efforts: throughExtraHigh, defaultEffort: "medium" },
    "gemini-3.8-flash": { efforts: throughHigh, defaultEffort: "medium" },
  },
};

export function exampleModelEfforts(selection: ModelSelection | null) {
  return selection ? examples[selection.provider][selection.modelId] : undefined;
}
