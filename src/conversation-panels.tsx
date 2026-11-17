import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { IDockviewPanelProps } from "dockview-react";
import { Activity, ArrowDown, ArrowUp, MessageSquare, Pencil, Plus, RotateCcw, Search, Target } from "lucide-react";
import { Dialog } from "radix-ui";
import { useCallback, useContext, useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type FormEvent } from "react";
import type { Conversation } from "@/bindings/Conversation";
import type { AdmissionBlocker } from "@/bindings/AdmissionBlocker";
import type { Job } from "@/bindings/Job";
import type { JobInspection } from "@/bindings/JobInspection";
import type { RecoveryAction } from "@/bindings/RecoveryAction";
import type { ConversationHistory } from "@/bindings/ConversationHistory";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import type { RunRecord } from "@/bindings/RunRecord";
import { ModelPicker } from "@/components/model-picker";
import { ModelCapabilityPreview } from "@/model-capability-preview";
import { EffortSelect } from "@/components/effort-select";
import { availableModelSelection, liveModelsQuery, sameModelSelection } from "@/live-models";
import { useCheckoutModelDefaults, useExecutionProfileDefault } from "@/model-defaults";
import { ConversationModeSelect, completesSideQuestion, goalCommand, sideQuestionCommand,
  sideQuestionCommandPrefix, type ConversationMode } from "@/conversation-mode";
import { ConversationGoalCard, GoalSettingsDialog, goalDefinition, hasCurrentGoal, useConversationGoal, type GoalDefinition } from "@/conversation-goal";
import { ConversationContinuationDialog, ConversationContinuationSettings, confirmConversationContinuation, submitConversationContinuation,
  type ConversationContinuationReview } from "@/conversation-continuation";
import { Button } from "@/components/ui/button";
import { ConversationActivity, ConversationRequests, StopConversation, isLiveRun, useConversationRun } from "@/conversation-run";
import { ConversationInstructions, useConversationInstructions } from "@/conversation-instructions";
import { ConversationHistoryControls, ConversationMessageHistoryActions, type ConversationMessageHistoryAction } from "@/conversation-history-controls";
import { ConversationSubagentControls } from "@/subagent-quota";
import { SideQuestionPanel, type SideSubmission } from "@/conversation-side-requests";
import { ConversationSuggestionButton, useConversationSuggestion } from "@/conversation-suggestions";
import { ConversationCompactionControl, useConversationCompaction } from "@/conversation-compaction";
import { ConversationMarkdown, type ConversationCode } from "@/conversation-markdown";
import { ConversationCodeFileDialog } from "@/conversation-code-file";
import { registerActiveCustomizationDraft } from "@/active-customization-draft";
import { readDraft, saveDraft } from "@/conversation-draft-writes";
import { ConversationRow } from "@/conversation-menu";
import { useConversationTranscriptExport } from "@/conversation-transcript-export";
import { ConversationReferenceChips, type ConversationReferencePreview } from "@/conversation-reference";
import { ConversationAttachments, useConversationAttachmentReview, useConversationAttachments } from "@/conversation-attachments";
import { ComposerCustomizationPicker, conversationSkillMessage, hasComposerSkills, useComposerCustomizations,
  type ComposerCustomization } from "@/conversation-customization-picker";
import {
  ConversationLibraryNavigation, ConversationReferencePicker, ConversationTitleEditor,
  conversationLibraryKey, useConversationRename, useConversationTitle,
} from "@/conversation-library";
import { ConversationComposerToolbar, ConversationEmptyState, ConversationMessage, ConversationTitleBar, ConversationTranscript } from "@/conversation-layout";
import { WorktreeEvidence } from "@/worktree-evidence";
import { openConversation } from "@/workspace-tabs";
import { useWorkspace } from "@/workspace-state";
import { RunInspectorNavigation } from "@/run-inspector-navigation";
import { ConversationAuthoring, workflowAuthoringTarget } from "@/workflow-authoring";
import { openWorkspaceFile } from "@/file-context-actions";
import { reviewTerminalInputTarget } from "@/terminal-reviewed-input";

const historyOffsets = new Map<string, number>();

function pendingJob(inspection: JobInspection | null | undefined) {
  return Boolean(inspection && (!inspection.job.latestAttempt || inspection.job.queuedRecovery || inspection.queuedQuestion ||
    (inspection.job.downstreamWait && inspection.job.downstreamWait.state !== "yielding")) &&
    inspection.job.queueState !== "cancelled");
}

function waitReason(blocker: AdmissionBlocker | undefined): string {
  switch (blocker?.type) {
    case "queue_paused": return "The job queue is paused.";
    case "job_paused": return "This job is paused.";
    case "context_changed": return "The conversation changed since this message was queued.";
    case "conversation_busy": return "Waiting for the active conversation run.";
    case "instruction_order": return "Waiting for an earlier instruction.";
    case "continuation_owner": return "Waiting for the owning run to continue.";
    case "instruction_delivery_uncertain": return "An instruction in this context needs delivery review.";
    case "capacity": return `Waiting for an execution slot (${blocker.active} of ${blocker.limit} in use).`;
    case "policy": return blocker.reason;
    case "dependency": return "Waiting for a required job to complete.";
    case "downstream": return blocker.needsReview
      ? "The previous attempt needs review before this job can continue."
      : `Waiting for ${blocker.pending} downstream job results.`;
    case "approval": return "Waiting for approval.";
    case "writer": return blocker.needsReview
      ? "The previous worktree writer needs review."
      : "Waiting for the worktree writer.";
    case "app_writer": return "Waiting for an app write to finish.";
    case "pending_worktree_changes": return "The worktree has pending changes for another task scope.";
    case "missing_capability": return "The selected runner is missing a required capability.";
    case "job_cancelled": return "Job cancelled.";
    default: return "Waiting to start.";
  }
}

export function ConversationListPanel({ containerApi }: IDockviewPanelProps) {
  const checkoutId = useWorkspace((state) => state.checkout?.checkoutId);
  if (!checkoutId) return null;
  return <ConversationSidebar checkoutId={checkoutId} onOpen={(conversation) => openConversation(containerApi, conversation)} />;
}

export function useNewConversation(checkoutId: string, onOpen: (conversation: Conversation) => void) {
  const queryClient = useQueryClient();
  const key = ["conversations", checkoutId];
  return useMutation({
    mutationKey: ["new-conversation", checkoutId],
    scope: { id: "new-conversation:" + checkoutId },
    mutationFn: async () => {
      const list = await queryClient.fetchQuery({
        queryKey: key,
        queryFn: () => invoke<Conversation[]>("list_conversations", { checkoutId }),
      });
      return invoke<Conversation>("create_conversation", { checkoutId, title: `Conversation ${list.length + 1}` });
    },
    onSuccess: (conversation) => {
      void queryClient.invalidateQueries({ queryKey: key });
      void queryClient.invalidateQueries({ queryKey: conversationLibraryKey });
      onOpen(conversation);
    },
  });
}

export function ConversationSidebar({ checkoutId, onOpen, activeId, onRenamed }: {
  checkoutId: string;
  onOpen: (conversation: Conversation) => void;
  activeId?: string;
  onRenamed?: (conversation: Conversation) => void;
}) {
  const [filter, setFilter] = useState("");
  const [notice, setNotice] = useState("");
  const exportTranscript = useConversationTranscriptExport(setNotice);
  const [renaming, setRenaming] = useState<Conversation>();
  const [title, setTitle] = useState("");
  const sidebar = useRef<HTMLElement>(null);
  const listKey = ["conversations", checkoutId] as const;
  const conversations = useQuery({
    queryKey: listKey,
    queryFn: () => invoke<Conversation[]>("list_conversations", { checkoutId }),
    enabled: Boolean(checkoutId),
  });
  const start = useNewConversation(checkoutId, onOpen);
  const rename = useConversationRename(renaming, (updated) => {
    onRenamed?.(updated);
    setRenaming(undefined);
  });
  const error = start.error ?? exportTranscript.error ?? conversations.error;
  const matching = conversations.data?.filter(({ title }) => title.toLowerCase().includes(filter.trim().toLowerCase()));
  return (
    <section ref={sidebar} className="conv-sessions" aria-label="Conversation list">
      <header className="conv-session-heading">
        <h2>Conversations</h2>
        <button className="conv-icon-button" type="button" title="New conversation" aria-label="New conversation"
          onClick={() => start.mutate()} disabled={conversations.isPending || start.isPending}>
          <Plus aria-hidden /><span className="sr-only">New conversation</span>
        </button>
      </header>
      <label className="conv-search">
        <Search aria-hidden /><input aria-label="Filter conversations" placeholder="Filter conversations" value={filter}
          onChange={(event) => setFilter(event.target.value)} />
      </label>
      {error && <p role="alert" className="conv-alert mt-2">{String(error)}</p>}
      {conversations.isPending && <p role="status" className="text-sm text-muted-foreground">Loading conversations…</p>}
      {matching?.length === 0 && <p className="conv-sidebar-note">{filter ? "No matching conversations." : "New conversations appear here."}</p>}
      <ul className="conv-session-list">
        {matching?.map((conversation) => (
          <li key={conversation.id}>
            <ConversationRow title={conversation.title} available={(action) => ["open", "copy-id", "copy-title", "rename"].includes(action) ||
              (action === "export-markdown" && !exportTranscript.isPending)}
              onAction={(action) => {
                if (action === "open") {
                  onOpen(conversation);
                  return;
                }
                if (action === "rename") {
                  setRenaming(conversation);
                  setTitle(conversation.title);
                  rename.reset();
                  return;
                }
                if (action === "export-markdown") {
                  exportTranscript.mutate(conversation);
                  return;
                }
                const title = action === "copy-title";
                void Promise.resolve().then(() => navigator.clipboard.writeText(title ? conversation.title : conversation.id))
                  .then(() => setNotice(title ? "Title copied." : "Conversation ID copied."))
                  .catch(() => setNotice("Couldn’t copy to the clipboard."));
              }}>
              <button type="button" className="conv-session-button" data-conversation-id={conversation.id}
                aria-current={activeId === conversation.id ? "page" : undefined}
                onClick={() => onOpen(conversation)}>
                <MessageSquare aria-hidden /><span>{conversation.title}</span>
              </button>
            </ConversationRow>
          </li>
        ))}
      </ul>
      {notice && <p role="status" className="conv-sidebar-note">{notice}</p>}
      {renaming && <Dialog.Root open onOpenChange={(open) => { if (!open && !rename.isPending) setRenaming(undefined); }}>
        <Dialog.Portal>
          <Dialog.Overlay className="conv-dialog-scrim" />
          <Dialog.Content className="conv-dialog" onCloseAutoFocus={(event) => {
            const row = Array.from(sidebar.current?.querySelectorAll<HTMLElement>("[data-conversation-id]") ?? [])
              .find(({ dataset }) => dataset.conversationId === renaming.id);
            const focus = row ?? sidebar.current?.querySelector<HTMLElement>('input[aria-label="Filter conversations"]');
            if (focus) { event.preventDefault(); focus.focus(); }
          }}>
            <Dialog.Title>Rename conversation</Dialog.Title>
            <Dialog.Description>{renaming.title}</Dialog.Description>
            <form className="conv-dialog-form" onSubmit={(event) => {
              event.preventDefault();
              if (title.trim() && !rename.isPending) rename.mutate(title.trim());
            }}>
              <label>Conversation title<input type="text" value={title} required disabled={rename.isPending}
                onChange={(event) => setTitle(event.currentTarget.value)} /></label>
              {rename.error && <p role="alert" className="conv-alert">{String(rename.error)}</p>}
              <footer>
                <Dialog.Close asChild><button type="button" className="conv-quiet-button" disabled={rename.isPending}>Cancel</button></Dialog.Close>
                <button type="submit" className="conv-primary-button" disabled={!title.trim() || rename.isPending}>
                  {rename.isPending ? "Saving…" : "Save"}
                </button>
              </footer>
            </form>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>}
    </section>
  );
}

type PanelParams = { conversationId: string; title: string };
const selectorClass = "conv-select";

export function ConversationPanel({ params, api, containerApi }: IDockviewPanelProps<PanelParams>) {
  const library = useContext(ConversationLibraryNavigation);
  const selectedCheckout = useWorkspace((state) => state.checkout?.checkoutId);
  const checkoutId = library?.checkoutId ?? selectedCheckout;
  const savedTitle = useConversationTitle(checkoutId, params.conversationId);
  const title = savedTitle.data?.title ?? params.title;
  const subscribe = useCallback((update: () => void) => {
    const listener = api.onDidVisibilityChange(update);
    return () => listener.dispose();
  }, [api]);
  const visible = useSyncExternalStore(subscribe, () => api.isVisible);
  const subscribeActive = useCallback((update: () => void) => {
    const listener = containerApi?.onDidActivePanelChange?.(update);
    return () => listener?.dispose();
  }, [containerApi]);
  const active = useSyncExternalStore(subscribeActive, () => Boolean(containerApi?.activePanel && containerApi.activePanel.id === api.id));
  useEffect(() => {
    if (api.renderer !== "always") api.setRenderer("always");
  }, [api]);
  useEffect(() => {
    if (title === params.title) return;
    api.setTitle(title);
    api.updateParameters({ title });
  }, [api, params.title, title]);
  return <ConversationContent key={params.conversationId} conversationId={params.conversationId}
    title={title} checkoutId={checkoutId} confirmedCheckoutId={savedTitle.data?.checkoutId} visible={visible} active={active}
    openFork={(conversation) => { if (containerApi) openConversation(containerApi, conversation); }} />;
}

type ReviewedContinuation = {
  review: ConversationContinuationReview;
  selection: ModelSelection;
  effort: string | null;
  mode: "read_only" | "plan";
  text: string;
};

function ConversationContent({ conversationId, title, checkoutId, confirmedCheckoutId, visible, active, openFork }: PanelParams & {
  checkoutId?: string; confirmedCheckoutId?: string; visible: boolean; active: boolean; openFork: (conversation: Conversation) => void;
}) {
  const queryClient = useQueryClient();
  const library = useContext(ConversationLibraryNavigation);
  const [renamingTitle, setRenamingTitle] = useState(false);
  const renameTrigger = useRef<HTMLButtonElement>(null);
  const draftKey = ["conversation", conversationId, "draft"];
  const draft = useQuery({
    queryKey: draftKey,
    queryFn: () => readDraft(conversationId),
    staleTime: Infinity,
  });
  const referenceDraftKey = ["conversation", conversationId, "reference-draft"];
  const referenceDraft = useQuery({
    queryKey: referenceDraftKey,
    queryFn: () => invoke<ConversationReferencePreview[]>("conversation_reference_draft", { conversationId }),
    enabled: Boolean(library) && visible,
  });
  const editReferenceDraft = useMutation({
    mutationFn: ({ previewId, attach }: { previewId: string; attach: boolean }) =>
      invoke<ConversationReferencePreview[]>("edit_conversation_reference_draft", { conversationId, previewId, attach }),
    onSuccess: (references) => queryClient.setQueryData(referenceDraftKey, references),
    onError: () => queryClient.invalidateQueries({ queryKey: referenceDraftKey }),
  });
  const references = referenceDraft.data ?? [];
  const hasReferences = references.length > 0;
  const historyKey = ["conversation", conversationId, "messages"];
  const history = useQuery({
    queryKey: historyKey,
    queryFn: () => invoke<ConversationHistory>("conversation_history", { conversationId }),
  });
  const historyRegion = useRef<HTMLDivElement>(null);
  const historyContents = useRef<HTMLDivElement>(null);
  const stickToEnd = useRef(!historyOffsets.has(conversationId));
  const [showJump, setShowJump] = useState(false);
  useLayoutEffect(() => {
    if (!visible || !history.data || !historyRegion.current) return;
    const region = historyRegion.current;
    region.scrollTop = stickToEnd.current ? region.scrollHeight : historyOffsets.get(conversationId) ?? 0;
  }, [conversationId, visible, history.data]);
  useEffect(() => {
    const contents = historyContents.current;
    const region = historyRegion.current;
    if (!visible || !contents || !region) return;
    const observer = new ResizeObserver(() => {
      if (stickToEnd.current) region.scrollTop = region.scrollHeight;
    });
    observer.observe(contents);
    return () => observer.disconnect();
  }, [visible]);
  const activeEdit = useRef(0);
  const [draftError, setDraftError] = useState("");
  const [draftStatus, setDraftStatus] = useState("");
  const [providerOverride, setProvider] = useState<Provider>();
  const [explicitSelection, setSelection] = useState<ModelSelection | null>(null);
  const [effort, setEffort] = useState("");
  const [mode, setMode] = useState<ConversationMode>("read_only");
  const [continuation, setContinuation] = useState<ReviewedContinuation>();
  const [continuationCheckoutId, setContinuationCheckoutId] = useState<string>();
  const [historyAction, setHistoryAction] = useState<ConversationMessageHistoryAction>();
  const dismissHistoryAction = useCallback(() => setHistoryAction(undefined), []);
  const text = draft.data ?? "";
  const sideQuestion = sideQuestionCommand(text);
  const isSideQuestion = sideQuestion !== undefined;
  const sideCompletion = completesSideQuestion(text);
  const composer = useRef<HTMLTextAreaElement>(null);
  const composerForm = useRef<HTMLFormElement>(null);
  const attachments = useConversationAttachments(conversationId, Boolean(library), visible, composerForm);
  const hasAttachments = attachments.draft.files.length > 0;
  const [composerCursor, setComposerCursor] = useState(0);
  const hasSkills = hasComposerSkills(text);
  const [showSideQuestions, setShowSideQuestions] = useState(false);
  const goalInput = goalCommand(text);
  const composingGoal = !isSideQuestion && (mode === "goal" || goalInput !== undefined);
  const { overview: goal, refresh: refreshGoal } = useConversationGoal(conversationId, visible);
  const [goalSettings, setGoalSettings] = useState<{ objective: string; submittedDraft?: string }>();
  const checkoutDefaults = useCheckoutModelDefaults(checkoutId, visible);
  const projectModel = mode === "plan" ? checkoutDefaults.data?.planning : checkoutDefaults.data?.conversation;
  const provider = explicitSelection?.provider ?? providerOverride ?? projectModel?.provider ?? "codex";
  const models = useQuery(liveModelsQuery(provider));
  const modelOptions = models.data?.map(({ option }) => option) ?? [];
  const profileId = modelOptions.find(({ selectable }) => selectable)?.selection.profileId;
  const profileDefault = useExecutionProfileDefault(provider, profileId, visible && !explicitSelection);
  const selection = explicitSelection ?? availableModelSelection(modelOptions, projectModel, profileDefault.data);
  const selected = models.data?.find(({ option }) => sameModelSelection(option.selection, selection));
  const antigravityModel = provider === "antigravity" ? selected?.option.selection.modelId : undefined;
  const modelEfforts = useQuery({
    ...liveModelsQuery(provider, antigravityModel),
    enabled: Boolean(antigravityModel),
  });
  const efforts = antigravityModel
    ? modelEfforts.data?.find(({ option }) => sameModelSelection(option.selection, selection))
    : selected;
  const jobKey = ["conversation", conversationId, "latest-job"];
  const latestJob = useQuery({
    queryKey: jobKey,
    queryFn: () => invoke<JobInspection | null>("latest_conversation_job", { conversationId }),
    refetchInterval: (query) => pendingJob(query.state.data) || isLiveRun(query.state.data?.job.latestAttempt) ? 250 : false,
  });
  const job = latestJob.data;
  const queued = pendingJob(job);
  const attempt = job?.job.latestAttempt;
  const runKey = ["conversation", conversationId, "latest-run"];
  const latest = useQuery({
    queryKey: runKey,
    queryFn: () => invoke<RunRecord | null>("latest_conversation_run", { conversationId }),
    refetchInterval: (query) => isLiveRun(query.state.data) ? 250 : false,
  });
  const run = latest.data;
  const instructions = useConversationInstructions(conversationId, visible);
  const cancelRequested = attempt?.runId === run?.id && Boolean(attempt?.cancelRequested);
  const activity = useConversationRun(conversationId, run, cancelRequested);
  const { live } = activity;
  const busy = queued || live || isLiveRun(attempt) || activity.savedQuestions.length > 0;
  const [inspectedRun, setInspectedRun] = useState<string>();
  const activityButton = useRef<HTMLButtonElement>(null);
  const inspector = useContext(RunInspectorNavigation);
  const selectedCheckout = useWorkspace((state) => state.checkout);
  const codeCheckout = selectedCheckout?.checkoutId === confirmedCheckoutId ? selectedCheckout : undefined;
  const [codeFile, setCodeFile] = useState<{ checkoutId: string; block: ConversationCode }>();
  const [codeFileStatus, setCodeFileStatus] = useState("");
  const showingActivity = visible && run && inspectedRun === run.id;
  const openRunEvidence = (runId: string) => {
    if (inspector) inspector.openActivity(runId, conversationId);
    else setInspectedRun(runId);
  };
  useEffect(() => {
    if (!visible) setInspectedRun(undefined);
  }, [visible]);
  function closeActivity() {
    setInspectedRun(undefined);
    activityButton.current?.focus();
  }
  useEffect(() => {
    if (attempt?.runId) void queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "latest-run"] });
  }, [attempt?.runId, conversationId, queryClient]);
  useEffect(() => {
    if (run?.status) void queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "messages"] });
  }, [run?.id, run?.status, conversationId, queryClient]);
  function clearSubmittedDraft(submitted: string) {
    activeEdit.current++;
    queryClient.setQueryData<string>(draftKey, (current) => current === submitted ? "" : current);
    setDraftStatus("");
    setDraftError("");
  }
  const effectiveEffort = efforts?.efforts.includes(effort) ? effort : null;
  const movingCheckout = continuationCheckoutId && continuationCheckoutId !== checkoutId;
  const compactionEnabled = Boolean(library && visible && selected?.option.selectable && mode !== "goal" && !movingCheckout);
  const compaction = useConversationCompaction({ conversationId, selection, effort: effectiveEffort, mode: mode === "goal" ? "read_only" : mode,
    pendingText: text, enabled: compactionEnabled });
  const attachmentReview = useConversationAttachmentReview({
    conversationId, revision: attachments.draft.revision, contextId: history.data?.contextId,
    selection, effort: effectiveEffort, mode, text, enabled: visible && Boolean(selected?.option.selectable),
    targetCheckoutId: continuationCheckoutId,
  });
  const customizationPicker = useComposerCustomizations({
    conversationId, selection, effort: effectiveEffort, mode: mode === "goal" ? "read_only" : mode,
    draft: text, cursor: composerCursor, enabled: visible && !composingGoal && !isSideQuestion,
  });
  const steeringPreview = useQuery({
    queryKey: ["conversation", conversationId, "steering-route", run?.id, history.data?.contextId, selection, effectiveEffort, mode],
    queryFn: () => invoke<boolean>("preview_conversation_steering", { conversationId, runId: run?.id,
      expectedContextId: history.data?.contextId, selection, effort: effectiveEffort, mode }),
    enabled: visible && live && run?.status === "running" && Boolean(history.data?.contextId && selected?.option.selectable)
      && Boolean(text.trim()) && !isSideQuestion && !composingGoal && !hasReferences && !hasSkills && !hasAttachments,
    retry: false,
    refetchInterval: 1250,
  });
  const steer = useMutation({
    mutationFn: ({ instructionId, runId }: { instructionId: string; runId: string }) =>
      invoke<boolean>("steer_queued_conversation_instruction", { conversationId, runId, instructionId }),
    onSettled: instructions.refresh,
  });
  const send = useMutation({
    mutationFn: async ({ text: submitted }: { text: string; toRun?: string }): Promise<ReviewedContinuation | { instructionId: string } | null> => {
      let expectedContextId = history.data?.contextId;
      if (!selection || !expectedContextId || mode === "goal") throw new Error("Choose a model and wait for the conversation context.");
      if (compactionEnabled && !busy) {
        const reviewedContext = await compaction.beforeSend(expectedContextId, submitted);
        if (!reviewedContext) return null;
        if (reviewedContext !== expectedContextId) {
          await queryClient.invalidateQueries({ queryKey: historyKey });
          if (hasAttachments) return null;
          expectedContextId = reviewedContext;
        }
      }
      const effective = { selection, effort: effectiveEffort, mode };
      const referenceIds = references.length > 0 ? references.map(({ id }) => id) : undefined;
      const attachmentPreviewId = attachmentReview.review?.previewId;
      if (hasAttachments && !attachmentPreviewId) throw new Error("Review the files before sending.");
      await saveDraft(conversationId, submitted);
      const review = await submitConversationContinuation({ conversationId, expectedContextId, ...effective, text: submitted,
        ...(referenceIds && { referenceIds }), ...(attachmentPreviewId && { attachmentPreviewId }),
        ...(continuationCheckoutId && { targetCheckoutId: continuationCheckoutId }) });
      if (review.status === "review_required") return { review, ...effective, text: submitted } satisfies ReviewedContinuation;
      return { instructionId: review.job.id };
    },
    onSuccess: async (outcome, { text: submitted, toRun }) => {
      if (!outcome) return;
      if ("review" in outcome) { setContinuation(outcome); return; }
      clearSubmittedDraft(submitted);
      void instructions.refresh();
      if (toRun) steer.mutate({ instructionId: outcome.instructionId, runId: toRun });
      if (library) await Promise.all([queryClient.invalidateQueries({ queryKey: referenceDraftKey }), attachments.refresh()]);
    },
    onError: () => {
      if (library) void queryClient.invalidateQueries({ queryKey: referenceDraftKey });
      if (hasAttachments) attachmentReview.refresh();
    },
  });
  const continueSend = useMutation({
    mutationFn: ({ choice, intent }: { choice: Parameters<typeof confirmConversationContinuation>[0]["choice"]; intent: ReviewedContinuation }) =>
      confirmConversationContinuation({ conversationId, previewId: intent.review.previewId, choice,
        selection: intent.selection, effort: intent.effort, mode: intent.mode, text: intent.text }),
    onSuccess: async ({ conversation }, { intent }) => {
      clearSubmittedDraft(intent.text);
      setContinuation(undefined);
      void instructions.refresh();
      void queryClient.invalidateQueries({ queryKey: conversationLibraryKey });
      if (library && intent.review.targetCheckout) {
        setContinuationCheckoutId(undefined);
        await library.open(intent.review.targetCheckout, conversation.id);
      } else if (conversation.id !== conversationId) openFork(conversation);
      if (library) await Promise.all([queryClient.invalidateQueries({ queryKey: referenceDraftKey }), attachments.refresh()]);
    },
    onError: () => { if (library) void queryClient.invalidateQueries({ queryKey: referenceDraftKey }); },
  });
  const startGoal = useMutation({
    mutationFn: async ({ definition, submittedDraft }: { definition: GoalDefinition; submittedDraft?: string }) => {
      const contextId = history.data?.contextId;
      if (!selection || !selected?.option.selectable || !contextId) throw new Error("Choose a model and wait for the conversation context.");
      if (submittedDraft !== undefined) await saveDraft(conversationId, submittedDraft);
      return invoke("start_conversation_goal", {
        conversationId, expectedContextId: contextId, selection, effort: efforts?.efforts.includes(effort) ? effort : null,
        definition, submittedDraft: submittedDraft ?? null,
      });
    },
    onSuccess: async (_, { submittedDraft }) => {
      if (submittedDraft !== undefined && queryClient.getQueryData(draftKey) === submittedDraft) {
        activeEdit.current++;
        queryClient.setQueryData(draftKey, "");
        setDraftStatus("");
      }
      setGoalSettings(undefined);
      await Promise.all([refreshGoal(), queryClient.invalidateQueries({ queryKey: jobKey }), instructions.refresh()]);
    },
  });
  const cancelQueued = useMutation({
    mutationFn: (jobId: string) => invoke("cancel_conversation_job", { conversationId, jobId }),
    onSettled: () => queryClient.invalidateQueries({ queryKey: jobKey }),
  });
  const askSide = useMutation({
    mutationFn: (submitted: string) => invoke<SideSubmission>("enqueue_conversation_side_request", {
      conversationId, expectedContextId: history.data?.contextId, selection,
      effort: efforts?.efforts.includes(effort) ? effort : null,
      mode: mode === "goal" ? "read_only" : mode,
      purpose: "question", question: sideQuestionCommand(submitted) ?? "",
    }),
    onSuccess: (outcome, submitted) => {
      if (outcome.status === "queued") {
        if (queryClient.getQueryData(draftKey) === submitted) editDraft("");
        void queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "side-requests"] });
      }
    },
  });
  const sideEffort = efforts?.efforts.includes(effort) ? effort : null;
  const sideMode = mode === "goal" ? "read_only" : mode;
  const validateSide = useMutation({
    mutationFn: () => invoke<SideSubmission>("enqueue_conversation_side_request", {
      conversationId, expectedContextId: history.data?.contextId, selection,
      effort: sideEffort, mode: sideMode, purpose: "validation", question: "",
    }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["conversation", conversationId, "side-requests"] }),
  });
  const suggestion = useConversationSuggestion({
    conversationId, contextId: history.data?.contextId, run, selection, effort: sideEffort, mode,
    draft: text, draftVersion: activeEdit.current, visible, liveFresh: latest.isFetchedAfterMount,
    eligible: !busy && draft.isSuccess && !send.isPending && !continueSend.isPending && !startGoal.isPending
      && !hasReferences && !hasAttachments && !hasSkills && !isSideQuestion && !composingGoal && Boolean(selected?.option.selectable),
  });
  const recover = useMutation({
    mutationFn: ({ jobId, runId, action }: { jobId: string; runId: string; action: RecoveryAction }) =>
      invoke<Job>("recover_conversation_job", { conversationId, jobId, runId, action }),
    onSuccess: (recoveryJob) => {
      queryClient.setQueryData<JobInspection>(jobKey, { job: recoveryJob, blockers: [], recoveryActions: [] });
      void queryClient.invalidateQueries({ queryKey: jobKey });
    },
  });

  function editDraft(text: string) {
    const edit = ++activeEdit.current;
    setDraftError("");
    setDraftStatus("Saving draft…");
    queryClient.setQueryData(draftKey, text);
    void saveDraft(conversationId, text).then(() => {
      if (edit === activeEdit.current) setDraftStatus(text ? "Draft saved" : "");
    }, (error: unknown) => {
      if (edit === activeEdit.current) {
        setDraftStatus("");
        setDraftError(String(error));
      }
    });
  }

  function appendToDraft(markdown: string) {
    const current = queryClient.getQueryData<string>(draftKey) ?? "";
    const separator = !current || current.endsWith("\n\n") ? "" : current.endsWith("\n") ? "\n" : "\n\n";
    editDraft(`${current}${separator}${markdown}\n\n`);
    requestAnimationFrame(() => composer.current?.focus());
  }

  function quoteInDraft(text: string) {
    appendToDraft(text.split("\n").map((line) => `> ${line}`).join("\n"));
  }

  function copyMessageText(text: string) {
    void Promise.resolve().then(() => navigator.clipboard.writeText(text)).catch(() => setDraftError("Could not copy to the clipboard."));
  }

  function pickCustomization(option: ComposerCustomization) {
    const next = customizationPicker.pick(option);
    editDraft(next.text);
    setComposerCursor(next.cursor);
    requestAnimationFrame(() => {
      composer.current?.focus();
      composer.current?.setSelectionRange(next.cursor, next.cursor);
    });
  }

  const canStartGoal = Boolean(history.data?.contextId && selected?.option.selectable && !busy && !latest.isPending && !latestJob.isPending
    && !goal.isPending && !goal.error && !hasCurrentGoal(goal.data) && !send.isPending && !recover.isPending);
  const referencesReady = !library || (referenceDraft.isSuccess && !editReferenceDraft.isPending);
  const canSend = referencesReady && attachments.ready && !attachments.busy
    && (!hasAttachments || (!composingGoal && !isSideQuestion && Boolean(attachmentReview.review?.previewId)))
    && !customizationPicker.pinned.some(({ option }) => !option)
    && (composingGoal ? Boolean(!hasReferences && !hasSkills && text.trim() && !startGoal.isPending && (goalInput === "" || canStartGoal))
    : Boolean(text.trim() && selected?.option.selectable && (isSideQuestion
    ? !hasReferences && !hasSkills && Boolean(sideQuestion) && history.data?.contextId && !askSide.isPending
    : history.data?.contextId && !latest.isPending && !latestJob.isPending && !send.isPending && !recover.isPending)));
  const queueNext = busy || instructions.data.some((instruction) => !instruction.recordedContext);
  const historyBusy = live || isLiveRun(attempt) || send.isPending || continueSend.isPending || steer.isPending;
  const canReceiveCustomization = active && visible && Boolean(library) && !busy && !historyBusy && draft.isSuccess
    && !composingGoal && !isSideQuestion && !movingCheckout;
  const customizationSelection = selected?.option.selectable ? selection : null;
  useEffect(() => {
    if (!canReceiveCustomization || !checkoutId || checkoutId !== confirmedCheckoutId || mode === "goal") return;
    return registerActiveCustomizationDraft({ checkoutId, conversationId, title, selection: customizationSelection, effort: effectiveEffort, mode });
  }, [canReceiveCustomization, checkoutId, confirmedCheckoutId, conversationId, title, customizationSelection, effectiveEffort, mode]);
  const steerRun = canSend && !hasReferences && !hasAttachments && !hasSkills && !steer.isPending && !isSideQuestion && !composingGoal && run?.status === "running"
    && steeringPreview.data && !instructions.data.some((item) => item.editable && !item.recordedContext) ? run.id : undefined;

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!canSend) return;
    if (isSideQuestion) {
      setShowSideQuestions(true);
      askSide.mutate(text);
      return;
    }
    if (composingGoal) {
      if (goalInput === "") {
        startGoal.reset();
        setGoalSettings({ objective: "", submittedDraft: text });
      } else {
        startGoal.mutate({ definition: goalDefinition(goalInput ?? text.trim()), submittedDraft: text });
      }
      return;
    }
    stickToEnd.current = true;
    if (historyRegion.current) historyRegion.current.scrollTop = historyRegion.current.scrollHeight;
    setShowJump(false);
    steer.reset();
    send.mutate({ text });
  }

  function completeSideQuestion() {
    editDraft(sideQuestionCommandPrefix);
    composer.current?.focus();
  }

  return (
    <section className="conv-pane" aria-label={title}>
      <ConversationTitleBar title={title}
        editor={renamingTitle && checkoutId ? <ConversationTitleEditor conversation={{ id: conversationId, checkoutId, title }}
          onClose={() => { setRenamingTitle(false); requestAnimationFrame(() => renameTrigger.current?.focus()); }} /> : undefined}
        rename={checkoutId && <button ref={renameTrigger} className="conv-icon-button" type="button" aria-label="Rename conversation"
          title="Rename conversation" onClick={() => setRenamingTitle(true)}><Pencil aria-hidden /></button>}>
        {library && inspector?.openWorkspace && <ConversationAuthoring projectId={library.projectId} conversationId={conversationId}
          selectedModel={selected?.option.selectable ? selection : null} effort={sideEffort}
          onOpenDraft={(draftId) => inspector.openWorkspace?.("pipelines", workflowAuthoringTarget(draftId))} />}
        <ConversationContinuationSettings conversationId={conversationId} targetCheckoutId={continuationCheckoutId}
          onCheckout={library ? setContinuationCheckoutId : undefined} />
        <button type="button" className="conv-quiet-button" onClick={() => { startGoal.reset(); setGoalSettings({ objective: "" }); }}>
          <Target size={14} aria-hidden /> Goal
        </button>
        <button type="button" className="conv-quiet-button" aria-expanded={showSideQuestions}
          onClick={() => setShowSideQuestions(!showSideQuestions)}><MessageSquare aria-hidden /> Side questions</button>
        {run && <button ref={activityButton} type="button" className="conv-quiet-button"
          aria-expanded={inspector ? inspector.activeRunId === run.id : Boolean(showingActivity)}
          onClick={() => {
            if (inspector) inspector.openActivity(run.id, conversationId);
            else if (showingActivity) closeActivity();
            else setInspectedRun(run.id);
          }}>
          <Activity aria-hidden /> Activity
        </button>}
        <ConversationHistoryControls conversationId={conversationId} title={title} history={history.data}
          disabled={historyBusy} openFork={openFork} mode={mode} selection={selection ?? undefined}
          effort={sideEffort ?? undefined} requestedAction={historyAction} onRequestedActionClose={dismissHistoryAction} />
        <ConversationSubagentControls conversationId={conversationId} runId={attempt?.runId} />
      </ConversationTitleBar>
      <div className="conv-layout">
      <div className="conv-chat">
      <ConversationTranscript historyRef={historyRegion} contentRef={historyContents}
        onScroll={(event) => {
          const region = event.currentTarget;
          if (!visible || !region.clientHeight) return;
          historyOffsets.set(conversationId, region.scrollTop);
          stickToEnd.current = region.scrollHeight - region.clientHeight - region.scrollTop < 32;
          setShowJump(!stickToEnd.current);
        }}>
        {history.isPending && <p role="status" className="text-sm text-muted-foreground">Loading messages…</p>}
        {history.error && <p role="alert" className="conv-alert">{String(history.error)}</p>}
        {history.data?.messages.length === 0 && !live && !queued && <ConversationEmptyState title="Start a conversation"
          description="Choose a model and ask about this repository." />}
        <ol className="conv-messages">
          {history.data?.messages.map((message) => (
            <ConversationMessage key={message.revisionId} role={message.role}
              selectionMenu={(selection) => [[
                { label: "Copy selected text", run: () => copyMessageText(selection) },
                { label: "Quote selected text in draft", disabled: busy || historyBusy || !draft.isSuccess, run: () => quoteInDraft(selection) },
              ]]}
              menu={[[
                { label: "Copy message as Markdown", run: () => {
                  const markdown = message.role === "user" ? conversationSkillMessage(message.text) : message.text;
                  copyMessageText(markdown);
                } },
                { label: "Copy message revision ID", run: () => copyMessageText(message.revisionId) },
                { label: "Quote in draft", disabled: busy || historyBusy || !draft.isSuccess, run: () => {
                  const markdown = message.role === "user" ? conversationSkillMessage(message.text) : message.text;
                  quoteInDraft(markdown);
                } },
              ], [
                { label: "Rewind here…", disabled: historyBusy, run: () => setHistoryAction({ kind: "rewind", revisionId: message.revisionId }) },
                { label: "Fork from message…", disabled: historyBusy, run: () => setHistoryAction({ kind: "fork", revisionId: message.revisionId }) },
                ...(message.role === "user" ? [{ label: "Edit in a fork…", disabled: historyBusy,
                  run: () => setHistoryAction({ kind: "edit", revisionId: message.revisionId }) }] : []),
              ]]}
              actions={<ConversationMessageHistoryActions message={message} disabled={historyBusy} onAction={setHistoryAction} />}>
              {message.role === "user" ? <p>{conversationSkillMessage(message.text)}</p> : <ConversationMarkdown
                codeActions={{
                  addToDraft: appendToDraft, draftDisabled: busy || historyBusy || !draft.isSuccess,
                  saveFile: codeCheckout ? (block) => setCodeFile({ checkoutId: codeCheckout.checkoutId, block }) : undefined,
                  reviewTerminal: codeCheckout && inspector?.openWorkspace ? (block) => {
                    if (useWorkspace.getState().checkout?.checkoutId !== codeCheckout.checkoutId) return;
                    inspector.openWorkspace?.("ide", reviewTerminalInputTarget(codeCheckout.checkoutId, block.text));
                  } : undefined,
                }}>
                {message.text}
              </ConversationMarkdown>}
            </ConversationMessage>
          ))}
        </ol>
        {(latestJob.error || cancelQueued.error || recover.error) && <p role="alert" className="conv-alert">{String(latestJob.error || cancelQueued.error || recover.error)}</p>}
        {codeFileStatus && <p role="status" className="text-xs text-muted-foreground">{codeFileStatus}</p>}
        {latest.error && <p role="alert" className="conv-alert">{String(latest.error)}</p>}
        {activity.error && <p role="alert" className="conv-alert">{String(activity.error)}</p>}
        {live && activity.output && <div className="conv-stream" aria-label="Streaming response">
          <ConversationMarkdown codeActions={{}}>{activity.output}</ConversationMarkdown>
        </div>}
        {run && <ConversationRequests activity={activity} conversationId={conversationId} runId={run.id} />}
        {job?.job.mode === "approved_write" && attempt && !isLiveRun(attempt) && (
          <WorktreeEvidence key={attempt.runId} projectId={job.job.projectId} runId={attempt.runId} ownerTaskId={job.job.worktreeOwnerTaskId} />
        )}
        {job && attempt && !busy && job.recoveryActions.length > 0 && (
          <section className="conv-recovery" aria-label="Recover job">
            <div className="conv-recovery-message"><RotateCcw size={16} aria-hidden="true" /><div>
              <h2>Recover this turn</h2>
              <p>{job.recoveryActions.includes("resume") ? "Continue from a recorded checkpoint or retry with the saved input." : "Start another attempt with the saved input."}</p>
            </div></div>
            <div className="flex flex-wrap gap-2">
            {job.recoveryActions.map((action) => <Button key={action} type="button" variant="outline" disabled={recover.isPending || send.isPending}
              onClick={() => recover.mutate({ jobId: job.job.id, runId: attempt.runId, action })}>
              {action === "retry" ? "Retry saved input" : "Resume from checkpoint"}
            </Button>)}
            </div>
          </section>
        )}
      </ConversationTranscript>
      <div className="conv-composer-area">
        {goal.data && <ConversationGoalCard value={goal.data} refresh={refreshGoal} openRun={openRunEvidence} />}
        {showJump && <button type="button" className="conv-jump" onClick={() => {
          stickToEnd.current = true;
          if (historyRegion.current) historyRegion.current.scrollTop = historyRegion.current.scrollHeight;
          setShowJump(false);
        }}><ArrowDown aria-hidden /> Jump to latest</button>}
        {Boolean(activity.inputs.length || activity.savedQuestions.length) && <button className="conv-request-link" type="button" onClick={() => {
          const request = historyRegion.current?.querySelector<HTMLElement>('[aria-label="Pending requests"]');
          request?.scrollIntoView({ block: "nearest" });
          (request?.querySelector<HTMLElement>("button, input, select") ?? request)?.focus({ preventScroll: true });
        }}>{job?.queuedQuestion ? "View answer" : "Waiting for input"} <ArrowUp aria-hidden /></button>}
        <ConversationInstructions conversationId={conversationId} instructions={instructions} openRun={openRunEvidence} />
        {queued && job && (job.job.downstreamWait || job.queuedQuestion || (job.job.queuedRecovery && !instructions.data.some(({ id }) => id === job.job.id))) && <div className="conv-queued">
          <strong>{job.job.downstreamWait ? "Waiting for downstream jobs" : job.queuedQuestion ? "Queued answer" : "Queued message"}</strong>
          <div>
            <p role="status" className="text-muted-foreground">{waitReason(job.blockers[0])}</p>
            <button type="button" className="conv-quiet-button" disabled={cancelQueued.isPending}
              onClick={() => cancelQueued.mutate(job.job.id)}>
              {cancelQueued.isPending ? "Cancelling…" : job.queuedQuestion ? "Cancel queued answer" : job.job.downstreamWait ? "Cancel downstream wait" : "Cancel queued message"}
            </button>
          </div>
        </div>}
      <form ref={composerForm} onSubmit={submit} className={`conv-composer ${attachments.hover ? "ring-2 ring-primary" : ""}`}>
          <ConversationAttachments attachments={attachments} preview={attachmentReview} checkoutId={confirmedCheckoutId}
            disabled={send.isPending || continueSend.isPending} />
          <ComposerCustomizationPicker picker={customizationPicker} onPick={pickCustomization}
            onRemove={(token) => editDraft(customizationPicker.remove(token))} />
          <ConversationReferenceChips references={references}
            disabled={editReferenceDraft.isPending || send.isPending || continueSend.isPending}
            onRemove={(previewId) => editReferenceDraft.mutate({ previewId, attach: false })} />
          {sideCompletion && <button type="button" className="conv-quiet-button" onClick={completeSideQuestion}>
            <code>/btw</code> Ask a side question
          </button>}
          {suggestion && <ConversationSuggestionButton suggestion={suggestion} onAccept={() => editDraft(suggestion.text)} />}
          <label htmlFor={`draft-${conversationId}`} className="sr-only">Message draft</label>
          <textarea ref={composer} id={`draft-${conversationId}`} placeholder="Ask about this repository…"
            aria-autocomplete="list"
            aria-controls={customizationPicker.options.length ? customizationPicker.listId : undefined}
            aria-activedescendant={customizationPicker.options.length ? `${customizationPicker.listId}-${customizationPicker.highlighted}` : undefined}
            value={text} onChange={(event) => {
              setComposerCursor(event.currentTarget.selectionStart);
              editDraft(event.currentTarget.value);
            }} onSelect={(event) => setComposerCursor(event.currentTarget.selectionStart)}
            disabled={draft.isPending || Boolean(draft.error) || send.isPending || startGoal.isPending}
            onKeyDown={(event) => {
              if (customizationPicker.active && !event.nativeEvent.isComposing) {
                const option = customizationPicker.options[customizationPicker.highlighted];
                if (event.key === "Escape") {
                  event.preventDefault();
                  customizationPicker.dismiss();
                  return;
                }
                if (option && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
                  event.preventDefault();
                  customizationPicker.move(event.key === "ArrowDown" ? 1 : -1);
                  return;
                }
                if (option && !event.shiftKey && !event.ctrlKey && !event.metaKey && (event.key === "Enter" || event.key === "Tab")) {
                  event.preventDefault();
                  pickCustomization(option);
                  return;
                }
              }
              if (sideCompletion && event.key === "Tab" && !event.shiftKey && !event.nativeEvent.isComposing) {
                event.preventDefault();
                completeSideQuestion();
                return;
              }
              if (suggestion && event.key === "Tab" && !event.shiftKey && !event.nativeEvent.isComposing) {
                event.preventDefault();
                editDraft(suggestion.text);
                return;
              }
              if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && !event.nativeEvent.isComposing) {
                event.preventDefault();
                event.currentTarget.form?.requestSubmit();
              }
            }} />
          <ConversationComposerToolbar actions={<>
            {live && <StopConversation {...activity} />}
            {steerRun && <Button type="button" variant="outline" onClick={() => { steer.reset(); send.mutate({ text, toRun: steerRun }); }}>
              Send to active turn
            </Button>}
            <button type="submit" className="conv-primary-button conv-send" disabled={!canSend}>
              {isSideQuestion ? askSide.isPending ? "Queueing…" : "Ask side question"
                : startGoal.isPending ? "Starting goal…" : goalInput === "" ? "Open Goal Mode" : composingGoal ? "Start goal"
                  : send.isPending ? "Queueing…" : queueNext ? "Queue for next turn" : "Send"} <ArrowUp className="size-3.5" aria-hidden />
            </button>
          </>}>
              {library && <ConversationReferencePicker projectId={library.projectId} excludeId={conversationId} onOpen={library.open}
                onAttach={referencesReady && !send.isPending && !continueSend.isPending
                  ? ({ id }) => editReferenceDraft.mutate({ previewId: id, attach: true }) : undefined} />}
              <select aria-label="Provider" className={selectorClass} value={provider} onChange={(event) => {
                setProvider(event.target.value as Provider);
                setSelection(null);
                setEffort("");
              }}>
                <option value="codex">Codex</option>
                <option value="claude">Claude Code</option>
                <option value="antigravity">Antigravity</option>
              </select>
              <ModelPicker label="Model" layout="inline" options={modelOptions} loading={models.isPending}
                previewDetails={(option) => <ModelCapabilityPreview option={option}
                  scope={run ? { projectId: run.projectId, conversationId, runId: run.id, live } : undefined}
                  openEvidence={run ? () => {
                    if (inspector) inspector.openActivity(run.id, conversationId);
                    else setInspectedRun(run.id);
                  } : undefined} />}
                selection={{ mode: "single", selected: selection, onChange: (model) => { setSelection(model); setEffort(""); } }} />
              <ConversationModeSelect mode={mode} onChange={setMode} />
              {compactionEnabled && <ConversationCompactionControl compaction={compaction} contextId={history.data?.contextId}
                disabled={busy || send.isPending || continueSend.isPending} />}
              <EffortSelect capabilities={efforts} loading={Boolean(antigravityModel && modelEfforts.isPending)}
                className={selectorClass} value={effort} onChange={setEffort} />
          </ConversationComposerToolbar>
          {goal.error && composingGoal && <p role="alert" className="conv-alert">{String(goal.error)}</p>}
          {startGoal.error && !goalSettings && <p role="alert" className="conv-alert">{String(startGoal.error)}</p>}
          {models.error && <div className="flex flex-wrap items-center gap-2">
            <p role="alert" className="conv-alert">{String(models.error)}</p>
            <Button type="button" variant="outline" onClick={() => void models.refetch()}>Refresh models</Button>
          </div>}
          {antigravityModel && modelEfforts.error && <div className="flex flex-wrap items-center gap-2">
            <p role="alert" className="conv-alert">Could not load effort levels for this model.</p>
            <Button type="button" variant="outline" onClick={() => void modelEfforts.refetch()}>Retry effort lookup</Button>
          </div>}
          {draftStatus && <p role="status" className="text-xs text-muted-foreground">{draftStatus}</p>}
          {(draftError || draft.error) && <p role="alert" className="conv-alert">{draftError || String(draft.error)}</p>}
          {(referenceDraft.error || editReferenceDraft.error) && <p role="alert" className="conv-alert">
            {String(referenceDraft.error || editReferenceDraft.error)}
          </p>}
          {hasReferences && (isSideQuestion || composingGoal) && <p className="conv-alert">
            Remove the conversation references to send a side question or start a goal.
          </p>}
          {hasAttachments && (isSideQuestion || composingGoal) && <p className="conv-alert">
            Remove the attached files to send a side question or start a goal.
          </p>}
          {hasSkills && (isSideQuestion || composingGoal) && <p className="conv-alert">
            Remove the selected skills to send a side question or start a goal.
          </p>}
          {send.error && <p role="alert" className="conv-alert">{String(send.error)}</p>}
          {askSide.error && <p role="alert" className="conv-alert">{String(askSide.error)}</p>}
          {steer.data === false && <p role="status" className="text-xs text-muted-foreground">Instruction queued for the next turn.</p>}
          {steer.isError && <p role="alert" className="conv-alert">{String(steer.error)}</p>}
      </form>
      </div>
      </div>
      {showingActivity && <ConversationActivity key={run.id} conversationId={conversationId} run={run} projection={activity.projection} onClose={closeActivity} />}
      {visible && showSideQuestions && <SideQuestionPanel conversationId={conversationId}
        submission={validateSide.data?.status === "unavailable" ? validateSide.data : askSide.data}
        validation={selection && selection.provider !== "antigravity" && selected?.option.selectable && history.data?.contextId
          ? { pending: validateSide.isPending, error: validateSide.error, onStart: () => { askSide.reset(); validateSide.mutate(); } }
          : undefined}
        onClose={() => { validateSide.reset(); setShowSideQuestions(false); }} />}
      </div>
      {visible && goalSettings && <GoalSettingsDialog objective={goalSettings.objective} options={models.data?.map(({ option }) => option) ?? []}
        selected={selection} onSelect={(model) => { setSelection(model); setEffort(""); }} value={goal.data} refresh={refreshGoal} openRun={openRunEvidence}
        pending={startGoal.isPending} error={startGoal.error ? String(startGoal.error) : undefined} canStart={canStartGoal}
        onStart={(definition) => startGoal.mutate({ definition, submittedDraft: goalSettings.submittedDraft })}
        onClose={() => { setGoalSettings(undefined); startGoal.reset(); }} />}
      {visible && continuation && <ConversationContinuationDialog key={continuation.review.previewId} review={continuation.review}
        pending={continueSend.isPending} error={continueSend.error} onConfirm={(choice) => continueSend.mutate({ choice, intent: continuation })}
        onClose={() => { setContinuation(undefined); continueSend.reset(); }} />}
      {visible && codeCheckout && codeFile?.checkoutId === codeCheckout.checkoutId && <ConversationCodeFileDialog checkout={codeCheckout} block={codeFile.block}
        close={() => setCodeFile(undefined)} created={(path) => {
          setCodeFileStatus(`Saved ${path}`);
          if (inspector?.openWorkspace && useWorkspace.getState().checkout?.checkoutId === codeCheckout.checkoutId) {
            openWorkspaceFile(inspector.openWorkspace, path, undefined, { repeat: true });
          }
        }} />}
    </section>
  );
}
