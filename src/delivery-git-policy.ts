export const defaultDeliveryGitPolicy = "automatic_commit_user_merge";

export const deliveryGitModes = [
  [defaultDeliveryGitPolicy, "Automatic commit and user merge"],
  ["user_commits_and_merges", "User commits and merges"],
] as const;

export type DeliveryGitPolicy = typeof deliveryGitModes[number][0];

export const simulatedDeliveryGitModes = [
  ...deliveryGitModes,
  ["automatic_local_commit_and_merge", "Automatic local commit and merge"],
] as const;

export type SimulatedDeliveryGitPolicy = typeof simulatedDeliveryGitModes[number][0];

export const deliveryGitWait = { commit: "Waiting for commit", merge: "Waiting for merge" } as const;

export const deliveryGitSetting = {
  id: "after-acceptance",
  label: "After accepting a diff",
  description: "The default creates a verified commit automatically. The user can start the merge. With user commits and merges, the commit step waits for a verified SHA.",
};
