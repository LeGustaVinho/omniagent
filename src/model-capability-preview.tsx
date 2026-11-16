import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { DropdownMenu } from "radix-ui";
import type { ModelOption } from "@/bindings/ModelOption";
import type { DispatchProfileEvidence } from "@/conversation-profile";

type RunScope = { projectId: string; conversationId: string; runId: string; live: boolean };

export function useRecordedCapabilityProfile(scope: RunScope | undefined, enabled: boolean) {
  const { projectId, conversationId, runId, live } = scope ?? {};
  return useQuery({
    queryKey: ["conversation-profile-evidence", projectId, conversationId, runId, live],
    enabled: Boolean(scope) && enabled,
    refetchInterval: ({ state }) => enabled && live && (!state.data || state.data.delivery === "prepared") ? 1250 : false,
    queryFn: () => invoke<DispatchProfileEvidence | null>("conversation_profile_evidence", { projectId, conversationId, runId }),
  });
}

export function ModelCapabilityPreview({ option, scope, openEvidence }: {
  option: ModelOption;
  scope?: RunScope;
  openEvidence?: () => void;
}) {
  const recorded = useRecordedCapabilityProfile(scope, true);
  const dispatch = recorded.data;
  const profileMatches = dispatch?.profile.provider === option.selection.provider
    && dispatch.profile.profileId === option.selection.profileId;
  const dispatched = dispatch?.profile.reportedAtDispatch.model ?? dispatch?.profile.requestedModel;
  const observed = dispatch?.modelObservations.findLast(({ reported, reassessment }) =>
    reported.model === option.selection.modelId && reassessment);
  const assessment = profileMatches
    ? observed?.reassessment?.assessment ?? (dispatched === option.selection.modelId ? dispatch?.assessment : undefined)
    : undefined;
  const statuses = new Map<string, number>();
  for (const { status } of assessment?.capabilities ?? []) {
    statuses.set(status, (statuses.get(status) ?? 0) + 1);
  }

  return <div className="model-picker-rating grid gap-1 text-xs" aria-label="Model capability details">
    <p>Model source · CLI inventory</p>
    <p className="break-all">Profile · {option.selection.profileId}</p>
    <p>Availability · {option.selectable ? "Selectable" : option.unavailableReason ?? "Unavailable"}</p>
    {scope && recorded.isPending && <p>Loading recorded capability evidence…</p>}
    {scope && !recorded.isPending && !assessment && <p>Recorded capability assessment unavailable for this model.</p>}
    {assessment && <>
      <p>Recorded CLI · {dispatch?.profile.cliVersion}</p>
      {statuses.size > 0 && <p>Recorded capability status · {Array.from(statuses, ([status, count]) => `${count} ${status}`).join(" · ")}</p>}
      {openEvidence && <DropdownMenu.Item className="model-picker-item" onSelect={openEvidence}>Inspect capability evidence</DropdownMenu.Item>}
    </>}
  </div>;
}
