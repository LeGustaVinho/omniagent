export type VerifiedMerge = {
  id: string;
  projectId: string;
  taskId: string;
  workflowRunId: string;
  unitId: string;
  commitEvidenceId: string;
  commitEvidenceIds: string[];
  sourceBranch: string;
  sourceSha: string;
  destinationBranch: string;
  previousDestinationSha: string;
  originalDestinationSha?: string | null;
  integrationSha: string;
  verifiedAt: string;
  source: string;
};

export type DeliveryMergePreview = {
  sourceBranch: string;
  sourceSha: string;
  destinationBranch: string;
  expectedDestinationSha: string;
};

export type MergeOutcome =
  | { status: "waiting_for_merge" }
  | { status: "destination_moved"; actualSha: string }
  | { status: "conflicted" }
  | { status: "destination_checked_out"; checkout: string }
  | { status: "destination_drift"; recovery: DestinationDriftOutcome }
  | { status: "merge_blocked"; reason: string }
  | { status: "recovery_required"; checkout: string; recoveryDirectory: string; message: string }
  | { status: "merge_verified"; evidence: VerifiedMerge }
  | { status: "artifact"; artifact: { id: string } };

export type MergeAttempt = (outcome: MergeOutcome) => void;

type DestinationDriftPreview = {
  id: string;
  sourceSha: string;
  destinationBranch: string;
  expectedDestinationSha: string;
  destinationSha: string;
  tree: string;
  changedPaths: string[];
};

type DestinationVerification = {
  id: string;
  gates: { gate: { name: string; required: boolean }; outcome: { status: string; reason?: string } | null }[];
};

export type AcceptedDestinationUpdate = {
  status: "accepted";
  preview: DestinationDriftPreview;
  verification: DestinationVerification;
  acceptanceId: string;
};

export type DestinationDriftOutcome =
  | { status: "paused"; expectedDestinationSha: string; actualSha: string }
  | { status: "preparing"; previewId: string }
  | { status: "checks_changed"; previewId: string }
  | { status: "conflicted"; previewId: string; destinationSha: string; paths: string[] }
  | { status: "checks_failed" | "requires_acceptance"; preview: DestinationDriftPreview; verification: DestinationVerification }
  | AcceptedDestinationUpdate
  | { status: "destination_moved"; expectedDestinationSha: string; actualSha: string | null };
