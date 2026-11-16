import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { SettingRow } from "@/settings-controls";

type GitPolicy = "automatic_commit_user_merge" | "user_commits_and_merges";

export function DeliveryGitSettings({ projectId }: { projectId: string }) {
  const client = useQueryClient();
  const queryKey = ["delivery-git-policy", projectId];
  const policy = useQuery({
    queryKey,
    queryFn: () => invoke<GitPolicy>("delivery_git_policy", { projectId }),
  });
  const save = useMutation({
    mutationFn: (policy: GitPolicy) => invoke("set_delivery_git_policy", { projectId, policy }),
    onSuccess: (_, saved) => client.setQueryData(queryKey, saved),
  });

  return <>
    <SettingRow id="after-acceptance" label="After accepting a diff"
      description="Choose who creates the verified commit. Merge is user-initiated.">
      <select id="after-acceptance" aria-describedby="after-acceptance-description"
        className="rounded-md border border-input bg-background px-2 py-1.5 text-sm"
        disabled={policy.data === undefined || save.isPending}
        value={policy.data ?? ""}
        onChange={(event) => save.mutate(event.target.value as GitPolicy)}>
        {policy.data === undefined && <option value="">Policy unavailable</option>}
        <option value="automatic_commit_user_merge">Automatic commit and user merge</option>
        <option value="user_commits_and_merges">User commits and merges</option>
      </select>
    </SettingRow>
    {!!(policy.error || save.error) && <p role="alert" className="settings-result settings-result-error">
      {String(save.error || policy.error)}
    </p>}
    {save.isSuccess && <p role="status" className="sr-only">Git policy saved</p>}
  </>;
}
