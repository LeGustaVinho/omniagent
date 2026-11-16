import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";

export type ModelDefaultPurpose = "conversation" | "planning" | "implementation";
export type ProjectModelDefaults = Record<ModelDefaultPurpose, ModelSelection | null>;

export const PROJECT_MODEL_DEFAULTS = ["project-model-defaults"];
export const CHECKOUT_MODEL_DEFAULTS = ["checkout-model-defaults"];
export const EXECUTION_PROFILE_MODEL = ["execution-profile-model"];

export function useProjectModelDefaults(projectId?: string, enabled = true) {
  return useQuery({ queryKey: [...PROJECT_MODEL_DEFAULTS, projectId], enabled: enabled && Boolean(projectId),
    queryFn: () => invoke<ProjectModelDefaults>("project_model_defaults", { projectId }), retry: false,
  });
}

export function useCheckoutModelDefaults(checkoutId?: string, enabled = true) {
  return useQuery({ queryKey: [...CHECKOUT_MODEL_DEFAULTS, checkoutId], enabled: enabled && Boolean(checkoutId),
    queryFn: () => invoke<ProjectModelDefaults>("checkout_model_defaults", { checkoutId }), retry: false,
  });
}

export function useExecutionProfileDefault(provider: Provider, profileId?: string, enabled = true) {
  return useQuery({ queryKey: [...EXECUTION_PROFILE_MODEL, provider, profileId], enabled: enabled && Boolean(profileId),
    queryFn: () => invoke<ModelSelection | null>("execution_profile_default_model", { provider, profileId }), retry: false,
  });
}
