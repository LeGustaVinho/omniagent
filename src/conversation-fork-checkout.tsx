import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";

export type ForkCheckoutChoice = "reuse_current_checkout" | "create_separate_worktree";
type ForkCheckoutPolicy = ForkCheckoutChoice | "ask_for_checkout";
type ForkCheckoutSettings = {
  projectDefault: ForkCheckoutPolicy;
  conversationOverride: ForkCheckoutPolicy | null;
  effective: ForkCheckoutPolicy;
};

export type ForkCheckoutPreview = {
  previewId: string | null;
  choice: ForkCheckoutChoice | null;
  projectId: string;
  repositoryRoot: string;
  checkoutId: string;
  checkoutRoot: string;
  baseCommit: string | null;
  dirtyPaths: string[];
  selectedPaths: string[];
  targetCheckoutRoot: string | null;
  targetBranch: string | null;
};

const labels = {
  reuse_current_checkout: "Reuse current checkout",
  create_separate_worktree: "Create a separate worktree",
  ask_for_checkout: "Ask for checkout",
} satisfies Record<ForkCheckoutPolicy, string>;

export function useForkCheckoutReview(conversationId: string, expectedContextId: string, enabled: boolean) {
  const client = useQueryClient();
  const [choice, setChoice] = useState<ForkCheckoutChoice | null>(null);
  const [selectedPaths, setSelectedPaths] = useState<string[]>([]);
  const settingsKey = ["conversation-fork-checkout-settings", conversationId];
  const previewKey = ["conversation-fork-checkout-preview", conversationId];
  const settings = useQuery({ queryKey: settingsKey, enabled,
    queryFn: () => invoke<ForkCheckoutSettings>("conversation_fork_checkout_settings", { conversationId }) });
  const review = useQuery({ queryKey: [...previewKey, expectedContextId, choice, selectedPaths], enabled,
    queryFn: () => invoke<ForkCheckoutPreview>("preview_conversation_fork_checkout", {
      conversationId, expectedContextId, choice, selectedPaths,
    }), refetchOnMount: "always", refetchOnWindowFocus: false, placeholderData: (previous) => previous });
  const save = useMutation({
    mutationFn: (input: { level: "project" | "conversation"; choice: ForkCheckoutPolicy | null }) =>
      invoke<ForkCheckoutSettings>("set_conversation_fork_checkout_policy", { conversationId, ...input }),
    onSuccess: async (next) => {
      setSelectedPaths([]);
      client.setQueryData(settingsKey, next);
      await client.invalidateQueries({ queryKey: previewKey });
    },
  });
  function choose(value: ForkCheckoutChoice | null) {
    setChoice(value);
    setSelectedPaths([]);
  }
  function togglePath(path: string, checked: boolean) {
    setSelectedPaths((current) => checked ? [...current, path].sort() : current.filter((entry) => entry !== path));
  }
  return { choice, choose, selectedPaths, togglePath, settings, review, save,
    confirmed: review.isSuccess && !review.isFetching && !review.isPlaceholderData && !save.isPending ? review.data : undefined };
}

export function ForkCheckoutReview({ form, disabled }: { form: ReturnType<typeof useForkCheckoutReview>; disabled: boolean }) {
  const { settings, review, save, choice, choose, selectedPaths, togglePath } = form;
  const preview = review.data;
  const lock = disabled || save.isPending;
  const error = settings.error ?? save.error ?? review.error;
  return <section aria-label="Fork checkout" className="grid gap-2">
    <label>Fork checkout <select value={choice ?? ""} disabled={lock}
      onChange={(event) => choose((event.currentTarget.value || null) as ForkCheckoutChoice | null)}>
      <option value="">{preview?.choice === null ? "Choose a checkout" : "Use saved policy"}</option>
      <option value="reuse_current_checkout">{labels.reuse_current_checkout}</option>
      <option value="create_separate_worktree">{labels.create_separate_worktree}</option>
    </select></label>
    {settings.data && <details><summary>Fork checkout policy</summary>
      <label>Project default <select disabled={lock} value={settings.data.projectDefault}
        onChange={(event) => save.mutate({ level: "project", choice: event.currentTarget.value as ForkCheckoutPolicy })}>
        {Object.entries(labels).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
      </select></label>
      <label>Conversation override <select disabled={lock} value={settings.data.conversationOverride ?? ""}
        onChange={(event) => save.mutate({ level: "conversation", choice: (event.currentTarget.value || null) as ForkCheckoutPolicy | null })}>
        <option value="">Use project default</option>
        {Object.entries(labels).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
      </select></label>
    </details>}
    {review.isFetching && <p role="status">Reviewing the checkout…</p>}
    {error && <p role="alert">{String(error)}</p>}
    {review.error && <button type="button" className="conv-quiet-button justify-self-start" disabled={lock}
      onClick={() => void review.refetch()}>Review checkout again</button>}
    {preview && <dl className="grid gap-1 break-all">
      <div><dt className="font-medium">Current checkout</dt><dd>{preview.checkoutRoot}</dd></div>
      {preview.baseCommit && <div><dt className="font-medium">Base commit</dt><dd><code>{preview.baseCommit}</code></dd></div>}
      {preview.targetCheckoutRoot && <div><dt className="font-medium">New worktree and branch</dt>
        <dd>{preview.targetCheckoutRoot} · {preview.targetBranch}</dd></div>}
    </dl>}
    {preview?.choice === "create_separate_worktree" && <fieldset disabled={lock}>
      <legend>Copy selected working changes</legend>
      {preview.dirtyPaths.length === 0 && <p>No working changes to copy.</p>}
      {preview.dirtyPaths.map((path) => <label key={path}><input type="checkbox" checked={selectedPaths.includes(path)}
        onChange={(event) => togglePath(path, event.currentTarget.checked)} /> {path}</label>)}
    </fieldset>}
  </section>;
}
