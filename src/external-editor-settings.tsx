import { invoke } from "@tauri-apps/api/core";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { SettingRow } from "@/settings-controls";

const settingKey = ["external-editor-setting"] as const;

export function ExternalEditorSettings() {
  const client = useQueryClient();
  const setting = useQuery({
    queryKey: settingKey,
    queryFn: () => invoke<string | null>("external_editor_setting"),
    retry: false,
  });
  const change = useMutation({
    mutationFn: async (action: "choose" | "clear") => {
      if (action === "clear") {
        await invoke("clear_external_editor");
        return null;
      }
      return (await invoke<string | null>("choose_external_editor")) ?? undefined;
    },
    onSuccess: (path) => { if (path !== undefined) client.setQueryData(settingKey, path); },
  });
  const error = change.error ?? setting.error;
  return <>
    <SettingRow id="external-editor-executable" label="External editor executable"
      description="Used by Open in external editor from the checkout file menus.">
      <input id="external-editor-executable" aria-describedby="external-editor-executable-description"
        type="text" readOnly value={setting.data ?? ""} placeholder={setting.isPending ? "Loading…" : "No editor selected"} />
    </SettingRow>
    <div className="settings-actions">
      <Button type="button" variant="outline" disabled={change.isPending}
        onClick={() => change.mutate("choose")}>Choose executable…</Button>
      <Button type="button" variant="ghost" disabled={!setting.data || change.isPending}
        onClick={() => change.mutate("clear")}>Clear external editor</Button>
    </div>
    {error && <p role="alert" className="settings-result settings-result-error">{String(error)}</p>}
  </>;
}
