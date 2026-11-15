import {
  Activity, ArrowDown, ArrowUp, FileText, GitFork, History, MessageSquare, Paperclip, Plus,
  Rows3, ShieldCheck, Target, X,
} from "lucide-react";
import { Dialog } from "radix-ui";
import { useLayoutEffect, useRef, useState, type FormEvent } from "react";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
import { ModelPicker } from "@/components/model-picker";
import { ConversationMarkdown } from "@/conversation-markdown";
import {
  deliveryConversation, demoModels, fileSamples, previewPlan, providers,
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

export function PreviewConversationPanel(props: PanelProps) {
  const { conversation, simulation, update, onFork, onRewind, openWorkspace, openActivity, openChanges } = props;
  const [mode, setMode] = useState("plan");
  const [provider, setProvider] = useState<Provider>("codex");
  const [selection, setSelection] = useState<ModelSelection | null>(demoModels.find(({ selection }) => selection.provider === "codex")?.selection ?? null);
  const [effort, setEffort] = useState("Standard");
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
  const modelOptions = demoModels.filter(({ selection }) => selection.provider === provider);
  const { draft, goal, sideQuestion } = conversation;
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

  function updateProfile(next: Provider) {
    setProvider(next);
    setSelection(demoModels.find(({ selection }) => selection.provider === next)?.selection ?? null);
    setDeliveryRoute(next === "codex" ? "live" : "queued");
  }

  function send(event: FormEvent) {
    event.preventDefault();
    const prompt = draft.trim();
    if (!prompt || !selection) return;
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
    if (prompt.startsWith("/goal ")) {
      setGoal({ objective: prompt.slice(6).trim(), state: "Running", spent: 0, budget: 3 });
      editDraft("");
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
      <header className="conv-heading">
        <MessageSquare aria-hidden /><h1>{conversation.title}</h1>
        {conversation.worktree !== "example-main" && <small className="min-w-0 truncate text-muted-foreground" title={conversation.worktree}>{conversation.worktree}</small>}
        {conversation.archived && <small className="text-muted-foreground">Archived</small>}
        <div className="conv-preview-header-controls">
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
        </div>
      </header>
      <div className="conv-layout">
        <div className="conv-chat">
          <div className="conv-history" ref={history} role="region" aria-label="Conversation history" onScroll={(event) => {
            const { scrollTop, scrollHeight, clientHeight } = event.currentTarget;
            stickToEnd.current = scrollHeight - clientHeight - scrollTop < 32;
            setShowJump(!stickToEnd.current);
          }}>
            <div className="conv-reading">
              {!conversation.messages.length && !step && <div className="conv-empty">
                <MessageSquare aria-hidden /><h2>Start a conversation</h2><p>Send a message or try /plan, /goal, and /btw.</p>
              </div>}
              <ol className="conv-messages">
                {conversation.messages.map((message) => (
                  <li key={message.id} className={message.author === "user" ? "conv-user conv-example-message" : "conv-assistant conv-example-message"}>
                    <p className="sr-only">{message.author === "user" ? "You" : "Assistant"}</p>
                    {message.author === "user" ? <p>{message.text}</p> : <ConversationMarkdown>{message.text}</ConversationMarkdown>}
                    <div className="conv-message-actions">
                      <button type="button" className="conv-quiet-button" onClick={() => onFork(message)}>Fork from here</button>
                      <button type="button" className="conv-quiet-button" onClick={() => onRewind(message)}>Rewind to here</button>
                    </div>
                  </li>
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
              {goal && <section className="conv-flow-card conv-example-flow" aria-label="Goal">
                <div className="conv-flow-heading"><span><Target aria-hidden /> Goal</span><small>{goal.state}</small></div>
                <p>{goal.objective}</p><progress value={goal.spent} max={goal.budget} aria-label="Goal turns used" />
                <small>{goal.spent} of {goal.budget} turns used · Managed continuation · Read-only profile</small>
                <div className="conv-route-actions">
                  {goal.state === "Running" && <>
                    <button type="button" className="conv-primary-button" onClick={() => setGoal({
                      ...goal, spent: goal.spent + 1, state: goal.spent + 1 === goal.budget ? "Completed" : "Running",
                    })}>Continue goal</button>
                    <button type="button" className="conv-quiet-button" onClick={() => setGoal({ ...goal, state: "Paused" })}>Pause goal</button>
                  </>}
                  {goal.state === "Paused" && <button type="button" className="conv-primary-button" onClick={() => setGoal({ ...goal, state: "Running" })}>Resume goal</button>}
                  {(goal.state === "Running" || goal.state === "Paused") && <button type="button" className="conv-quiet-button" onClick={() =>
                    setGoal({ ...goal, state: "Cancelled" })}>Cancel goal</button>}
                </div>
              </section>}
            </div>
          </div>
          <div className="conv-composer-area">
            {showJump && <button type="button" className="conv-jump" onClick={() => {
              if (history.current) history.current.scrollTop = history.current.scrollHeight;
              stickToEnd.current = true;
              setShowJump(false);
            }}><ArrowDown aria-hidden /> Jump to latest</button>}
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
                placeholder="Ask about this repository…"
                onKeyDown={(event) => {
                  if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && !event.nativeEvent.isComposing) {
                    event.preventDefault();
                    event.currentTarget.form?.requestSubmit();
                  }
                  if (event.key === "Tab" && !event.shiftKey && !draft && !activeRun && conversation.messages.length) {
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
              {!draft && !activeRun && conversation.messages.length > 0 && <button type="button" className="conv-suggestion"
                onClick={() => editDraft(exampleSuggestion)}>{exampleSuggestion}<kbd>Tab</kbd></button>}
              <div className="conv-composer-toolbar">
                <button type="button" className="conv-icon-button" title="Attach a file" aria-label="Attach a file" onClick={openAttachment}><Plus aria-hidden /></button>
                <select aria-label="Provider" className="conv-select" value={provider} onChange={(event) => updateProfile(event.currentTarget.value as Provider)}>
                  {Object.entries(providers).map(([id, name]) => <option key={id} value={id}>{name}</option>)}
                </select>
                <select aria-label="Profile" className="conv-select" value={mode} onChange={(event) => setMode(event.currentTarget.value)}>
                  <option value="read_only">Read-only</option><option value="plan">Planning</option>
                </select>
                <select aria-label="Thinking level" className="conv-select" value={effort} onChange={(event) => setEffort(event.currentTarget.value)}>
                  <option>Standard</option><option>High</option><option>Low</option>
                </select>
                {activeRun && <select aria-label="Delivery" className="conv-select" value={deliveryRoute} onChange={(event) => setDeliveryRoute(event.currentTarget.value)}>
                  <option value="live" disabled={provider !== "codex"}>Send to active turn</option>
                  <option value="queued">Queue for next turn</option>
                </select>}
                {activeRun && <button type="button" className="conv-quiet-button" onClick={() => simulation?.advanceDelivery("interrupt")}>Stop</button>}
                <button type="submit" className="conv-primary-button conv-send" disabled={!draft.trim() || !selection}>
                  {activeRun && deliveryRoute === "queued" ? "Queue" : "Send"} <ArrowUp size={15} aria-hidden />
                </button>
              </div>
              <div className="conv-model"><ModelPicker label="Model" options={modelOptions} selection={{ mode: "single", selected: selection, onChange: setSelection }} /></div>
              {queuedNotice && <p className="text-xs text-muted-foreground" role="status">{queuedNotice}</p>}
            </form>
          </div>
        </div>
        {showSideQuestion && sideQuestion && <aside className="conv-activity" aria-label="Side question" onKeyDown={(event) => {
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
        </aside>}
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
