import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useLayoutEffect, useSyncExternalStore, type CSSProperties } from "react";
import type { AppearanceSettings } from "@/bindings/AppearanceSettings";
import "@fontsource/spectral/latin-400.css";
import "./settings-appearance.css";

export const defaultAppearance: AppearanceSettings = {
  theme: "dark",
  interfaceFont: "geist",
  responseFont: "interface",
  conversationTextSize: "medium",
  transcriptWidth: "standard",
  motion: "system",
};

const queryKey = ["appearance-settings"] as const;
const darkQuery = "(prefers-color-scheme: dark)";
const reducedQuery = "(prefers-reduced-motion: reduce)";

function mediaMatches(query: string) {
  return window.matchMedia?.(query).matches ?? false;
}

function watchSystemAppearance(update: () => void) {
  const color = window.matchMedia?.(darkQuery);
  const motion = window.matchMedia?.(reducedQuery);
  color?.addEventListener("change", update);
  motion?.addEventListener("change", update);
  return () => {
    color?.removeEventListener("change", update);
    motion?.removeEventListener("change", update);
  };
}

function useResolvedAppearance(settings: AppearanceSettings) {
  const dark = useSyncExternalStore(watchSystemAppearance, () => mediaMatches(darkQuery));
  const reduced = useSyncExternalStore(watchSystemAppearance, () => mediaMatches(reducedQuery));
  return {
    theme: settings.theme === "system" ? (dark ? "dark" : "light") : settings.theme,
    motion: settings.motion === "reduced" || reduced ? "reduced" : "full",
  };
}

const transcriptWidths: Record<AppearanceSettings["transcriptWidth"], string> = {
  narrow: "640px",
  standard: "736px",
  wide: "896px",
};

const chatFontSizes: Record<AppearanceSettings["conversationTextSize"], string> = {
  small: "13px",
  medium: "14px",
  large: "16px",
};

export function useAppearancePresentation(settings: AppearanceSettings) {
  const { theme, motion } = useResolvedAppearance(settings);
  const interfaceFont =
    settings.interfaceFont === "segoe_ui" ? '"Segoe UI", sans-serif' : '"Geist Variable", "Geist", "Segoe UI", sans-serif';
  const responseFont =
    settings.responseFont === "spectral" ? '"Spectral", Georgia, serif' : interfaceFont;
  const style: CSSProperties & Record<string, string> = {
    "--app-interface-font": interfaceFont,
    "--app-response-font": responseFont,
    "--app-chat-font-size": chatFontSizes[settings.conversationTextSize],
    "--app-transcript-width": transcriptWidths[settings.transcriptWidth],
  };
  return { theme, motion, style };
}

function useAppearanceQuery(enabled = true) {
  return useQuery({
    queryKey,
    queryFn: () => invoke<AppearanceSettings>("appearance_settings"),
    enabled,
    staleTime: Infinity,
  });
}

export function useAppAppearance() {
  const preferences = useAppearanceQuery();
  const presentation = useAppearancePresentation(preferences.data ?? defaultAppearance);

  useLayoutEffect(() => {
    const root = document.documentElement;
    root.dataset.appTheme = presentation.theme;
    root.dataset.appMotion = presentation.motion;
    for (const [property, value] of Object.entries(presentation.style)) {
      root.style.setProperty(property, value);
    }
  }, [presentation]);
}

export function useEditableAppearance(enabled: boolean) {
  const client = useQueryClient();
  const preferences = useAppearanceQuery(enabled);
  const save = useMutation({
    mutationFn: (settings: AppearanceSettings) =>
      invoke<AppearanceSettings>("save_appearance_settings", { settings }),
    onMutate: (settings) => {
      const previous = client.getQueryData<AppearanceSettings>(queryKey);
      client.setQueryData(queryKey, settings);
      return previous;
    },
    onSuccess: (saved) => client.setQueryData(queryKey, saved),
    onError: (_error, _settings, previous) => {
      client.setQueryData(queryKey, previous ?? defaultAppearance);
    },
  });
  return { preferences, save };
}
