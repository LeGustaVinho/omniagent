import {
  Activity, ArrowDown, ArrowUp, FileText, Gauge, GitFork, History, MessageSquare, Paperclip, Pencil,
  Rows3, ShieldCheck, Target, X,
} from "lucide-react";
import { Dialog } from "radix-ui";
import { useLayoutEffect, useRef, useState, type FormEvent } from "react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ModelPicker } from "@/components/model-picker";
import { EffortSelect } from "@/components/effort-select";
import { ConversationComposerToolbar, ConversationEmptyState, ConversationMessage, ConversationSidePanel, ConversationTitleBar, ConversationTranscript } from "@/conversation-layout";
import { ConversationPipelineLabel, directConversationPipeline } from "@/conversation-pipeline-picker";
import { ConversationModeSelect, goalCommand, type ConversationMode } from "@/conversation-mode";
import { exampleModelEfforts } from "@/model-effort-examples";
import { ConversationMarkdown } from "@/conversation-markdown";
import {
  deliveryConversation, demoModels, fileSamples, previewPlan,
  type AttachmentRoute, type PreviewAttachment, type PreviewConversation, type PreviewGoal, type PreviewMessage,
} from "@/conversation-preview-data";
import type { DeliveryStep, WorkspaceProps } from "@/workbench-contract";

type ConversationReference = { id: string; title: string; project: string };
type PanelProps = WorkspaceProps & {
  conversation: PreviewConversation;
  references?: readonly ConversationReference[];
  update: (fn: (conversation: PreviewConversation) => PreviewConversation) => void;
  onFork: (message?: PreviewMessage) => void;
  onRewind: (message: PreviewMessage) => void;
};

type PanelDialog = "attachment" | "history" | "clear" | "compact" | "session";
const commandSuggestions = [
  { id: "goal", title: "/goal", text: "Start a persistent objective" },
  { id: "btw", title: "/btw", text: "Ask a separate read-only question" },
  { id: "review", title: "/review-checklist", text: "Apply the example review skill" },
  { id: "plan", title: "/plan", text: "Run in Planning mode" },
];
const exampleSuggestion = "Verify the recovery instructions against the release checklist.";

function addResponse(conversation: PreviewConversation, prompt: string, response?: string): PreviewConversation {
  const id = `message-${conversation.revision}`;
  return {
    ...conversation, draft: "", attachments: [], revision: conversation.revision + 1,
    messages: [...conversation.messages, { id, author: "user", text: prompt }, ...(response ? [
      { id: `${id}-response`, author: "assistant" as const, text: response },
    ] : [])],
  };
}

function visibleDeliveryStatus(step: DeliveryStep) {
  switch (step) {
    case "note": return "Ready to plan";
    case "question": return "Waiting for input";
    case "plan": return "Waiting for approval";
    case "queued": return "Queued";
    case "running": return "Running";
    case "review": return "Diff ready for review";
    case "verified": return "Verified · waiting for acceptance";
    case "accepted": return "Diff accepted";
    case "merged": return "Merged";
    case "interrupted": return "Interrupted";
    case "failed": return "Failed";
  }
}

function PreviewGoalCard({ goal, update }: { goal: PreviewGoal; update: (goal: PreviewGoal) => void }) {
  const [open, setOpen] = useState(false);
  const [objective, setObjective] = useState(goal.objective);
  const active = goal.state === "Running" || goal.state === "Paused";
  const revision = goal.previousObjectives.length + 1;

  function save(event: FormEvent) {
    event.preventDefault();
    const next = objective.trim();
    if (!active || !next || next === goal.objective) return;
    update({ ...goal, objective: next, previousObjectives: [...goal.previousObjectives, goal.objective] });
    setOpen(false);
  }

  return (
    <section className="conv-goal" aria-label="Goal">
      <Target size={16} aria-hidden />
      <div className="conv-goal-summary">
        <Dialog.Root open={open} onOpenChange={(next) => {
          if (next) setObjective(goal.objective);
          setOpen(next);
        }}>
          <Dialog.Trigger className="conv-goal-objective" type="button" aria-label="View goal objective" title={goal.objective}>
            <strong>{goal.objective}</strong>{active && <Pencil size={13} aria-hidden />}
          </Dialog.Trigger>
          <Dialog.Portal>
            <Dialog.Overlay className="conv-dialog-scrim" />
            <Dialog.Content className="conv-dialog" aria-describedby={undefined}>
              <Dialog.Title>Goal objective</Dialog.Title>
              <form className="conv-dialog-form" onSubmit={save}>
                <label>Objective
                  <textarea rows={6} value={objective} readOnly={!active} onChange={(event) => setObjective(event.currentTarget.value)} />
                </label>
                <p className="text-xs text-muted-foreground">Revision {revision} · {goal.spent}/{goal.budget} turns · {goal.model}</p>
                {goal.previousObjectives.length > 0 && <details className="conv-goal-revisions">
                  <summary>Previous revisions</summary>
                  <ol>{goal.previousObjectives.map((prior, index) => <li key={index}>
                    <small>Revision {index + 1}</small><p>{prior}</p>
                  </li>)}</ol>
                </details>}
                <footer>
                  <Dialog.Close className="conv-quiet-button" type="button">Close</Dialog.Close>
                  {active && <button type="submit" className="conv-primary-button"
                    disabled={!objective.trim() || objective.trim() === goal.objective}>Save objective</button>}
                </footer>
              </form>
            </Dialog.Content>
          </Dialog.Portal>
        </Dialog.Root>
        <small role="status">{goal.state} · {goal.spent}/{goal.budget} turns · {goal.model} · Simulation</small>
      </div>
      <div className="conv-goal-actions">
        {goal.state === "Running" && <>
          <button type="button" className="conv-quiet-button" onClick={() => update({
            ...goal, spent: goal.spent + 1, state: goal.spent + 1 === goal.budget ? "Budget reached" : "Running",
          })}>Continue goal</button>
          <button type="button" className="conv-quiet-button" onClick={() => update({ ...goal, state: "Paused" })}>Pause goal</button>
        </>}
        {goal.state === "Paused" && <button type="button" className="conv-quiet-button" onClick={() => update({ ...goal, state: "Running" })}>Resume goal</button>}
        {active && <button type="button" className="conv-quiet-button" onClick={() => update({ ...goal, state: "Cancelled" })}>Cancel goal</button>}
      </div>
    </section>
  );
}

export function PreviewConversationPanel(props: PanelProps) {
  const { conversation, simulation, update, onFork, onRewind, openWorkspace, openActivity, openChanges } = props;
  const [mode, setMode] = useState<ConversationMode>("plan");
  const [selection, setSelection] = useState<ModelSelection | null>(demoModels.find(({ selection }) => selection.provider === "codex")?.selection ?? null);
  const [effort, setEffort] = useState("");
  const [deliveryRoute, setDeliveryRoute] = useState("live");
  const [dialog, setDialog] = useState<PanelDialog>();
  const [attachment, setAttachment] = useState<PreviewAttachment>({ path: "docs/release.md", route: "Reference" });
  const [queueEdit, setQueueEdit] = useState<number>();
  const [queueValue, setQueueValue] = useState("");
  const [showSideQuestion, setShowSideQuestion] = useState(false);
  const [questionResponse, setQuestionResponse] = useState("Operators");
  const [sessionChoice, setSessionChoice] = useState("continue");
  const [queuedNotice, setQueuedNotice] = useState("");
  const [attachedReference, setAttachedReference] = useState<ConversationReference>();
  const [showJump, setShowJump] = useState(false);
  const history = useRef<HTMLDivElement>(null);
  const sideQuestionButton = useRef<HTMLButtonElement>(null);
  const stickToEnd = useRef(true);
  const step = conversation.id === deliveryConversation ? simulation?.delivery : undefined;
  const activeRun = step === "running";
  const provider = selection?.provider;
  const { draft, goal, sideQuestion } = conversation;
  const prompt = draft.trim();
  const goalInput = goalCommand(draft);
  const composingGoal = goalInput !== undefined || (mode === "goal" && prompt !== "/plan" && !prompt.startsWith("/btw "));
  const goalActive = goal?.state === "Running" || goal?.state === "Paused";
  const canSend = Boolean(draft.trim() && (goalInput === "" || (selection && (!composingGoal || !goalActive))));
  const isSlash = draft.startsWith("/") && !/\s/.test(draft);
  const isReference = draft.startsWith("@") && !draft.includes("\n") && !attachedReference;
  const suggestions = isSlash ? commandSuggestions.filter(({ title }) => title.includes(draft)) : [];
  const references = isReference ? props.references?.filter(({ id, title }) => draft.startsWith("@conversation:")
    ? id === draft.slice("@conversation:".length)
    : title.toLowerCase().includes(draft.slice(1).toLowerCase())) ?? [] : [];

  useLayoutEffect(() => {
    if (history.current && stickToEnd.current) history.current.scrollTop = history.current.scrollHeight;
  }, [conversation.messages.length, step]);

  function editDraft(value: string) {
    update((current) => ({ ...current, draft: value }));
  }

  function setGoal(next: PreviewGoal) {
    update((current) => ({ ...current, goal: next }));
  }

  function closeSideQuestion() {
    setShowSideQuestion(false);
    sideQuestionButton.current?.focus();
  }

  function chooseModel(model: ModelSelection) {
    setSelection(model);
    setEffort("");
    if (model.provider !== provider) setDeliveryRoute(model.provider === "codex" ? "live" : "queued");
  }

  function send(event: FormEvent) {
    event.preventDefault();
    if (goalInput === "") {
      setMode("goal");
      editDraft("");
      return;
    }
    if (!prompt || !selection) return;
    if (composingGoal) {
      const chosen = demoModels.find(({ selection: option }) => option.provider === selection.provider && option.modelId === selection.modelId && option.profileId === selection.profileId);
      if (!goalActive && chosen?.selectable) {
        setGoal({ objective: goalInput ?? prompt, previousObjectives: [], model: chosen.name, state: "Running", spent: 0, budget: 3 });
        setMode("goal");
        editDraft("");
      }
      return;
    }
    stickToEnd.current = true;
    setShowJump(false);
    if (prompt === "/plan") {
      setMode("plan");
      editDraft("");
      return;
    }
    if (prompt.startsWith("/btw ")) {
      update((current) => ({ ...current, sideQuestion: prompt.slice(5).trim(), draft: "" }));
      setShowSideQuestion(true);
      return;
    }
    const references = attachedReference ? `\n\nReferenced: ${attachedReference.project} / ${attachedReference.title}` : "";
    const attachments = conversation.attachments.length ? `\n\nAttachments: ${conversation.attachments.map(({ path, route }) => `${path} (${route})`).join(", ")}` : "";
    const recordedPrompt = prompt + references + attachments;
    if (activeRun && deliveryRoute === "queued") {
      update((current) => ({ ...current, queued: [...current.queued, recordedPrompt], draft: "", attachments: [] }));
      setQueuedNotice("Instruction queued for the next example turn.");
    } else {
      const response = step && ["note", "question", "plan"].includes(step) ? undefined : activeRun
        ? "I’ve added that instruction to the active example turn."
        : "The example turn recorded this request. Open **Activity** to inspect its scope and evidence.";
      update((current) => addResponse(current, recordedPrompt, response));
      if (step === "note") simulation?.advanceDelivery("start");
      setQueuedNotice("");
    }
    setAttachedReference(undefined);
  }

  function reply(event: FormEvent) {
    event.preventDefault();
    update((current) => addResponse(current, `Use ${questionResponse.toLowerCase()} as the audience.`));
    simulation?.advanceDelivery("answer");
  }

  function updateQueue(next: (queued: string[]) => string[]) {
    update((current) => ({ ...current, queued: next(current.queued) }));
    setQueueEdit(undefined);
  }

  function openAttachment() {
    setAttachment({ path: "docs/release.md", route: "Reference" });
    setDialog("attachment");
  }

  function changeSession() {
    if (sessionChoice === "fork") onFork();
    if (sessionChoice === "fresh") update((current) => ({
      ...current, former: [...current.former, current.messages], messages: [], revision: current.revision + 1,
    }));
    setDialog(undefined);
  }

  function replaceHistory(retained: PreviewMessage[]) {
    update((current) => ({ ...current, former: [...current.former, current.messages], messages: retained, revision: current.revision + 1 }));
    setDialog(undefined);
  }

  const dialogLabel = dialog ? {
    attachment: "Attach an example file", clear: "Clear conversation context", compact: "Compact context",
    history: "Previous context revisions", session: "Change the session profile",
  }[dialog] : "";
  const attachmentContent = fileSamples[attachment.path as keyof typeof fileSamples];
  return (
    <section className="conv-pane" aria-label={conversation.title}>
      <ConversationTitleBar title={conversation.title} details={<>
        <ConversationPipelineLabel pipeline={conversation.pipeline ?? directConversationPipeline} />
        {conversation.worktree !== "example-main" && <small className="conv-title-context" title={conversation.worktree}>{conversation.worktree}</small>}
        {conversation.archived && <small className="conv-title-context">Archived</small>}
      </>}>
          {sideQuestion && <button ref={sideQuestionButton} type="button" className="conv-quiet-button" aria-expanded={showSideQuestion}
            onClick={() => showSideQuestion ? closeSideQuestion() : setShowSideQuestion(true)}>
            <MessageSquare aria-hidden /> Side question
          </button>}
          <details className="conv-header-menu">
            <summary title="Conversation history actions"><History size={15} aria-hidden /><span className="sr-only">History actions</span></summary>
            <div>
              <button type="button" onClick={() => onFork()}><GitFork size={15} aria-hidden /> Create fork</button>
              <button type="button" onClick={() => setDialog("session")}><Rows3 size={15} aria-hidden /> Change session profile</button>
              <button type="button" onClick={() => setDialog("compact")}><FileText size={15} aria-hidden /> Compact context</button>
              <button type="button" onClick={() => setDialog("history")}><History size={15} aria-hidden /> Previous revisions</button>
              <button type="button" className="text-red-300" onClick={() => setDialog("clear")}><X size={15} aria-hidden /> Clear context</button>
            </div>
          </details>
          <button type="button" className="conv-quiet-button" onClick={() => openActivity("example-run")}>
            <Activity aria-hidden /> Activity
          </button>
      </ConversationTitleBar>
      <div className="conv-layout">
        <div className="conv-chat">
          <ConversationTranscript historyRef={history} onScroll={(event) => {
            const { scrollTop, scrollHeight, clientHeight } = event.currentTarget;
            stickToEnd.current = scrollHeight - clientHeight - scrollTop < 32;
            setShowJump(!stickToEnd.current);
          }}>
              {!conversation.messages.length && !step && <ConversationEmptyState title="Start a conversation"
                description="Send a message or try /plan, /goal, and /btw." />}
              <ol className="conv-messages">
                {conversation.messages.map((message) => (
                  <ConversationMessage key={message.id} role={message.author} actions={<>
                    <button type="button" className="conv-quiet-button" onClick={() => onFork(message)}>Fork from here</button>
                    <button type="button" className="conv-quiet-button" onClick={() => onRewind(message)}>Rewind to here</button>
                  </>}>
                    {message.author === "user" ? <p>{message.text}</p> : <ConversationMarkdown>{message.text}</ConversationMarkdown>}
                  </ConversationMessage>
                ))}
              </ol>
              {step && <section className="conv-example-flow" aria-label="Example delivery">
                {step === "note" && <div className="conv-route-actions">
                  <button type="button" className="conv-quiet-button" onClick={() => openWorkspace("knowledge", "docs/release.md")}><FileText aria-hidden /> View source note</button>
                  <button type="button" className="conv-primary-button" onClick={() => simulation?.advanceDelivery("start")}>Start planning</button>
                </div>}
                {step === "question" && <form onSubmit={reply} className="conv-flow-card">
                  <div className="conv-waiting">Waiting for input</div>
                  <strong>Who should the recovery instructions address?</strong>
                  <fieldset>
                    <legend className="sr-only">Audience</legend>
                    {["Operators", "Developers", "Both"].map((value) => <label key={value} className="conv-checkbox">
                      <input type="radio" name="delivery-audience" checked={questionResponse === value} onChange={() => setQuestionResponse(value)} /> {value}
                    </label>)}
                  </fieldset>
                  <button type="submit" className="conv-primary-button">Reply <ArrowUp size={15} aria-hidden /></button>
                </form>}
                {step !== "note" && step !== "question" && <article className="conv-flow-card">
                  <div className="conv-flow-heading"><span><FileText aria-hidden /> Plan revision 1</span><small>{visibleDeliveryStatus(step)}</small></div>
                  <ConversationMarkdown>{previewPlan}</ConversationMarkdown>
                  {step === "plan" && <div className="conv-route-actions">
                    <button type="button" className="conv-primary-button" onClick={() => simulation?.advanceDelivery("approve")}>
                      <ShieldCheck size={16} aria-hidden /> Approve implementation
                    </button>
                  </div>}
                  {!["note", "question", "plan"].includes(step) && <div className="conv-route-actions">
                    <button type="button" className="conv-quiet-button" onClick={() => openWorkspace("tasks", "release")}>Open release task</button>
                    {["review", "verified", "accepted", "merged"].includes(step) &&
                      <button type="button" className="conv-quiet-button" onClick={() => openChanges("saved")}>Open saved changes</button>}
                  </div>}
                </article>}
              </section>}
          </ConversationTranscript>
          <div className="conv-composer-area">
            {showJump && <button type="button" className="conv-jump" onClick={() => {
              if (history.current) history.current.scrollTop = history.current.scrollHeight;
              stickToEnd.current = true;
              setShowJump(false);
            }}><ArrowDown aria-hidden /> Jump to latest</button>}
            {goal && <PreviewGoalCard goal={goal} update={setGoal} />}
            {Boolean(conversation.queued.length) && <section className="conv-queued" aria-label="Queued instructions">
              <strong>Queued for next turn · {conversation.queued.length}</strong>
              {conversation.queued.map((entry, index) => <div key={index}>
                {queueEdit === index ? <>
                  <input aria-label="Queued instruction" value={queueValue} onChange={(event) => setQueueValue(event.currentTarget.value)} />
                  <button type="button" className="conv-quiet-button" disabled={!queueValue.trim()}
                    onClick={() => updateQueue((items) => items.map((item, position) => position === index ? queueValue.trim() : item))}>Save</button>
                </> : <>
                  <p>{entry}</p><button type="button" className="conv-quiet-button" onClick={() => { setQueueEdit(index); setQueueValue(entry); }}>Edit</button>
                </>}
                <button type="button" className="conv-icon-button" aria-label="Remove queued instruction"
                  onClick={() => updateQueue((items) => items.filter((_item, position) => position !== index))}><X aria-hidden /></button>
              </div>)}
              {!activeRun && <button type="button" className="conv-quiet-button" onClick={() => {
                update((current) => current.queued[0] ? {
                  ...addResponse(current, current.queued[0], "The queued instruction was delivered to the next example turn."),
                  queued: current.queued.slice(1),
                } : current);
              }}>Deliver next</button>}
            </section>}
            <form className="conv-composer" onSubmit={send} onDragOver={(event) => event.preventDefault()} onDrop={(event) => {
              event.preventDefault();
              openAttachment();
            }}>
              {(conversation.attachments.length > 0 || attachedReference) && <div className="conv-attachments">
                {conversation.attachments.map(({ path, route }) => <span key={path}><Paperclip aria-hidden />{path} · {route}
                  <button type="button" aria-label={`Remove ${path}`} onClick={() => update((current) => ({
                    ...current, attachments: current.attachments.filter((item) => item.path !== path),
                  }))}><X aria-hidden /></button>
                </span>)}
                {attachedReference && <span><MessageSquare aria-hidden />{attachedReference.title}
                  <button type="button" aria-label="Remove conversation reference" onClick={() => setAttachedReference(undefined)}><X aria-hidden /></button></span>}
              </div>}
              <textarea aria-label="Message draft" value={draft} onChange={(event) => editDraft(event.currentTarget.value)}
                placeholder={composingGoal ? (goalActive ? "A goal is already active" : "Describe the objective…") : "Ask about this repository…"}
                onKeyDown={(event) => {
                  if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && !event.nativeEvent.isComposing) {
                    event.preventDefault();
                    event.currentTarget.form?.requestSubmit();
                  }
                  if (event.key === "Tab" && !event.shiftKey && !draft && !activeRun && !composingGoal && conversation.messages.length) {
                    event.preventDefault();
                    editDraft(exampleSuggestion);
                  }
                }} />
              {suggestions.length > 0 && <div className="conv-completion" aria-label="Command suggestions">
                {suggestions.map(({ id, title, text }) => <button type="button" key={id} onClick={() => editDraft(`${title} `)}>
                  <strong>{title}</strong><span>{text}</span></button>)}
              </div>}
              {references.length > 0 && <div className="conv-completion" aria-label="Conversation references">
                {references.map((reference) => <button type="button" key={reference.id} onClick={() => {
                  setAttachedReference(reference);
                  editDraft("");
                }}><strong>{reference.title}</strong><span>{reference.project}</span></button>)}
              </div>}
              {!draft && !activeRun && !composingGoal && conversation.messages.length > 0 && <button type="button" className="conv-suggestion"
                onClick={() => editDraft(exampleSuggestion)}>{exampleSuggestion}<kbd>Tab</kbd></button>}
              <div className="conv-composer-tools"><button type="button" className="conv-quiet-button conv-composer-tool"
                aria-label="Attach a file" onClick={openAttachment}><Paperclip size={15} aria-hidden /> Attach files</button></div>
              <ConversationComposerToolbar actions={<>
                {activeRun && <button type="button" className="conv-quiet-button" onClick={() => simulation?.advanceDelivery("interrupt")}>Stop</button>}
                <button type="submit" className="conv-primary-button conv-send" disabled={!canSend}>
                  {goalInput === "" ? "Open Goal Mode" : composingGoal ? "Start goal" : activeRun && deliveryRoute === "queued" ? "Queue" : "Send"} <ArrowUp size={15} aria-hidden />
                </button>
              </>}>
                <ModelPicker label="Model" layout="inline" combinedCatalog options={demoModels}
                  selection={{ mode: "single", selected: selection, onChange: chooseModel }} />
                <ConversationModeSelect mode={mode} onChange={setMode} />
                <span className="conv-option"><Gauge size={15} aria-hidden />
                  <EffortSelect capabilities={exampleModelEfforts(selection)} className="conv-select" value={effort} onChange={setEffort} />
                </span>
                {activeRun && !composingGoal && <select aria-label="Delivery" className="conv-select" value={deliveryRoute} onChange={(event) => setDeliveryRoute(event.currentTarget.value)}>
                  <option value="live" disabled={provider !== "codex"}>Send to active turn</option>
                  <option value="queued">Queue for next turn</option>
                </select>}
              </ConversationComposerToolbar>
              {queuedNotice && <p className="text-xs text-muted-foreground" role="status">{queuedNotice}</p>}
            </form>
          </div>
        </div>
        {showSideQuestion && sideQuestion && <ConversationSidePanel label="Side question" onKeyDown={(event) => {
          if (event.key === "Escape") closeSideQuestion();
        }}>
          <header className="conv-activity-heading"><h2>Side question</h2>
            <button type="button" className="conv-icon-button ml-auto" aria-label="Close side question"
              onClick={closeSideQuestion}><X aria-hidden /></button>
          </header>
          <div className="conv-activity-content"><p>{sideQuestion}</p>
            <p className="text-sm text-muted-foreground">The simulator used a separate read-only turn. The release transcript still points to revision {conversation.revision}.</p>
            <button type="button" className="conv-quiet-button" onClick={() => {
              update((current) => ({ ...current, sideQuestion: undefined }));
              setShowSideQuestion(false);
            }}>Delete side question</button>
          </div>
        </ConversationSidePanel>}
      </div>
      {dialog && <Dialog.Root open onOpenChange={(open) => { if (!open) setDialog(undefined); }}>
        <Dialog.Portal><Dialog.Overlay className="conv-dialog-scrim" /><Dialog.Content className="conv-dialog">
          <Dialog.Title>{dialogLabel}</Dialog.Title>
          <Dialog.Description>Conversation revision {conversation.revision} · {conversation.project}</Dialog.Description>
          <div className="conv-dialog-form">
            {dialog === "attachment" && <>
              <label>File<select value={attachment.path} onChange={(event) => setAttachment({ ...attachment, path: event.currentTarget.value })}>
                {Object.keys(fileSamples).map((path) => <option key={path}>{path}</option>)}
              </select></label>
              <label>Delivery route<select value={attachment.route} onChange={(event) => setAttachment({ ...attachment, route: event.currentTarget.value as AttachmentRoute })}>
                <option>Reference</option><option>Excerpt</option><option>Native</option>
              </select></label>
              <p className="text-xs text-muted-foreground">{attachment.route === "Reference" ? "The example sends this checkout-relative path."
                : attachment.route === "Excerpt" ? "The example includes only the selected excerpt." : "The example includes this file as a native input."}</p>
              <pre className="conv-file-preview">{attachment.route === "Reference" ? attachment.path
                : attachment.route === "Excerpt" ? attachmentContent.split("\n").slice(0, 2).join("\n") : attachmentContent}</pre>
              <button type="button" className="conv-primary-button" onClick={() => {
                update((current) => ({ ...current, attachments: [...current.attachments.filter(({ path }) => path !== attachment.path), attachment] }));
                setDialog(undefined);
              }}>Attach file</button>
            </>}
            {dialog === "history" && (conversation.former.length ? conversation.former.map((messages, index) => <details key={index}>
              <summary>Previous revision {index + 1} · {messages.length} messages</summary>
              {messages.map(({ id, text }) => <p key={id} className="my-2 text-sm">{text}</p>)}
            </details>) : <p>No previous context revisions.</p>)}
            {dialog === "session" && <>
              <label>Continuation
                <select value={sessionChoice} onChange={(event) => setSessionChoice(event.currentTarget.value)}>
                  <option value="continue">Continue here</option><option value="fork">Fork with context</option><option value="fresh">Start fresh</option>
                </select>
              </label>
              <p>Example context: {sessionChoice === "fresh" ? "No retained messages" : `${conversation.messages.length} retained messages`}.</p>
              <button type="button" className="conv-primary-button" onClick={changeSession}>Apply continuation</button>
            </>}
            {dialog === "compact" && <>
              <p>Compact {Math.max(0, conversation.messages.length - 1)} older messages. The original remains available in previous context revisions.</p>
              <button type="button" className="conv-primary-button" disabled={conversation.messages.length < 2} onClick={() => replaceHistory([
                { id: `summary-${conversation.revision}`, author: "assistant", text: "**Summary:** The release checklist needs recovery instructions and a reviewed example patch." },
                ...conversation.messages.slice(-1),
              ])}>Compact</button>
            </>}
            {dialog === "clear" && <>
              <p>Start a fresh context for this conversation. The original revision remains available in previous context revisions.</p>
              <button type="button" className="conv-danger-button" onClick={() => replaceHistory([])}>Clear context</button>
            </>}
            <Dialog.Close asChild><button type="button" className="conv-quiet-button">Close</button></Dialog.Close>
          </div>
        </Dialog.Content></Dialog.Portal>
      </Dialog.Root>}
    </section>
  );
}
