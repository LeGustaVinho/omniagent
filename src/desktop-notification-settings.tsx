import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { SettingRow } from "@/settings-controls";

const notificationPreferenceKey = ["desktop-notifications-enabled"] as const;

export const desktopNotificationSetting = {
  id: "notifications",
  label: "Desktop notifications",
  description: "Notify when work needs input, waits for approval or finishes.",
};

export function DesktopNotificationSettings() {
  const client = useQueryClient();
  const preference = useQuery({
    queryKey: notificationPreferenceKey,
    queryFn: () => invoke<boolean>("desktop_notifications_enabled"),
    staleTime: Infinity,
  });
  const save = useMutation({
    mutationFn: (enabled: boolean) => invoke<boolean>("set_desktop_notifications_enabled", { enabled }),
    onSuccess: (enabled) => client.setQueryData(notificationPreferenceKey, enabled),
  });

  return <>
    <SettingRow {...desktopNotificationSetting}>
      <input id={desktopNotificationSetting.id} type="checkbox" aria-describedby="notifications-description"
        checked={preference.data ?? false} disabled={preference.data === undefined || save.isPending}
        onChange={(event) => save.mutate(event.target.checked)} />
    </SettingRow>
    {preference.isError && <p className="settings-result settings-result-error" role="alert">
      Desktop notification preference could not be loaded. <button type="button" onClick={() => void preference.refetch()}>Retry</button>
    </p>}
    {save.isError && <p className="settings-result settings-result-error" role="alert">
      Desktop notification preference could not be saved.
    </p>}
  </>;
}
