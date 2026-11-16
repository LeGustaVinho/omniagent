export const defaultDeliveryGitPolicy = "automatic_commit_user_merge";

export const deliveryGitModes = [
  [defaultDeliveryGitPolicy, "Automatic commit and user merge"],
  ["user_commits_and_merges", "User commits and merges"],
  ["automatic_local_commit_and_merge", "Automatic local commit and merge"],
] as const;

export type DeliveryGitPolicy = typeof deliveryGitModes[number][0];
export type DeliveryMilestonePolicy = { milestone: "verified_commit" | "verified_merge"; destinationBranch: string | null };
export type DestinationDriftPolicy = "pause" | "prepare_and_recheck";

export const deliveryGitWait = { commit: "Waiting for commit", merge: "Waiting for merge" } as const;
export const deliveryMilestoneKey = (projectId: string) => ["delivery-milestone-policy", projectId] as const;

export const deliveryGitSetting = {
  id: "after-acceptance",
  label: "After accepting a diff",
  description: "Choose who creates the verified commit and starts local integration. By default, OmniAgent commits and waits for the user to merge.",
};
