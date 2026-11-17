import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { CircleCheck, CircleMinus, LoaderCircle, ShieldCheck, ShieldQuestion, Terminal } from "lucide-react";
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
  const source = `CLI inventory · ${option.selection.profileId}`;
  const availability = option.selectable ? "Selectable" : option.unavailableReason ?? "Unavailable";
  const recordedStatus = assessment ? [dispatch?.profile.cliVersion && `CLI ${dispatch.profile.cliVersion}`,
    ...Array.from(statuses, ([status, count]) => `${count} ${status}`)].filter(Boolean).join(" · ") || "Capability evidence recorded"
    : recorded.isPending ? "Loading recorded capability evidence" : "No recorded capability evidence for this model";

  return <div className="model-capability-icons" role="group" aria-label="Model capability details">
    <span role="img" aria-label={source} title={source}><Terminal size={14} aria-hidden /></span>
    <span role="img" data-selectable={option.selectable} aria-label={availability} title={availability}>
      {option.selectable ? <CircleCheck size={14} aria-hidden /> : <CircleMinus size={14} aria-hidden />}
    </span>
    {scope && (assessment && openEvidence
      ? <DropdownMenu.Item className="model-capability-evidence" aria-label="Inspect capability evidence" aria-description={recordedStatus}
        title={recordedStatus} onSelect={openEvidence}><ShieldCheck size={15} aria-hidden /></DropdownMenu.Item>
      : <span role="img" aria-label={recordedStatus} title={recordedStatus}>
        {assessment ? <ShieldCheck size={14} aria-hidden /> : recorded.isPending ? <LoaderCircle size={14} aria-hidden /> : <ShieldQuestion size={14} aria-hidden />}
      </span>)}
  </div>;
}
