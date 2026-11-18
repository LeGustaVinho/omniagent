// @vitest-environment jsdom

import { EditorView } from "@codemirror/view";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ProjectTasksWorkspace } from "@/project-tasks";
import type { WorkspaceProps } from "@/workbench-contract";
import { testCheckout } from "@/test-fixtures/checkout";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const { projectId, checkoutId } = testCheckout;
const epic = { id: "epic-stable", title: "Release epic", owner: { epic: "epic-stable" } } as const;
const task = { id: "task-stable", title: "Write release notes", owner: { work: "task-stable" } } as const;
const subtask = { id: "subtask-stable", title: "Gather changes", owner: { work: "subtask-stable" } } as const;
type Work = typeof epic | typeof task | typeof subtask;
type Revision = { id: string; revision: number; markdown: string | null };
type Draft = { revision: string; base: Revision | null; markdown: string };
type State = { saved: Revision | null; draft: Draft | null; history: Map<string, Revision> };

function descriptionFixture() {
  const states = new Map<string, State>();
  for (const item of [epic, task, subtask]) {
    const saved = item === epic ? null : { id: `${item.id}-rev-1`, revision: 1, markdown: `# ${item.title}\n` };
    states.set(item.id, { saved, draft: null, history: new Map(saved ? [[saved.id, saved]] : []) });
  }
  let draftSequence = 0;
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    const request = args as Record<string, unknown> | undefined;
    switch (command) {
      case "project_work_hierarchy": return { epics: [epic], work: [
        { task, placement: { kind: "task", epicId: epic.id } },
        { task: subtask, placement: { kind: "subtask", parentTaskId: task.id } },
      ] };
      case "project_epic_delivery_state": return "in_progress";
      case "project_task_delivery_state": return { state: "in_progress", hasDirectWork: true, checklistResolved: false };
      case "project_task_checklist": return { items: [
        { id: "untouched", revision: 3, position: 0, body: "Workflow review", checked: false },
        { id: "complete", revision: 4, position: 1, body: "Workflow approval", checked: true },
      ] };
      case "project_task_deliveries": case "project_task_runs": case "project_task_assignments":
      case "project_task_goals": case "conversation_models": case "repository_note_catalog": return [];
      case "analyze_note_markdown": {
        const { markdown, bufferRevision } = request as { markdown: string; bufferRevision: string };
        const nodes = Array.from(markdown.matchAll(/^- \[([ x])\]/gm), ({ index, 1: status }) => ({
          kind: "task_marker", status, checked: status === "x", range: { from: index + 2, to: index + 5 },
          valueRange: { from: index + 3, to: index + 4 },
        }));
        return { bufferRevision, nodes };
      }
      case "project_description_editor":
      case "save_project_description_draft":
      case "save_project_description_editor": {
        const scope = request as {
          projectId: string; owner: Work["owner"]; baseRevision?: string | null; expectedRevision?: string | null;
          expectedDraftRevision?: string | null; markdown?: string | null;
        };
        expect(scope.projectId).toBe(projectId);
        const id = "epic" in scope.owner ? scope.owner.epic : scope.owner.work;
        const item = [epic, task, subtask].find((work) => work.id === id)!;
        expect(scope.owner).toEqual(item.owner);
        const state = states.get(id)!;
        const result = () => ({ saved: state.saved, draft: state.draft });
        if (command === "project_description_editor") return result();

        const expectedDraft = state.draft?.revision ?? null;
        if (command === "save_project_description_draft") {
          expect(scope.expectedDraftRevision).toBe(expectedDraft);
          const base = scope.baseRevision === null ? null : state.history.get(scope.baseRevision!)!;
          if (scope.baseRevision !== null) expect(base).toBeDefined();
          state.draft = { revision: `draft-${++draftSequence}`, base, markdown: scope.markdown! };
          return state.draft;
        }
        if (scope.expectedDraftRevision !== expectedDraft) {
          return { status: "conflict", reason: "draft_revision_changed", state: result() };
        }
        if (scope.expectedRevision !== (state.saved?.id ?? null)) {
          return { status: "conflict", reason: "saved_revision_changed", state: result() };
        }
        const revision = (state.saved?.revision ?? 0) + 1;
        state.saved = { id: `${id}-rev-${revision}`, revision, markdown: scope.markdown ?? null };
        state.history.set(state.saved.id, state.saved);
        state.draft = null;
        return { status: "saved", revision: state.saved };
      }
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
  return states;
}

function mountWorkspace() {
  const props: WorkspaceProps = {
    checkout: testCheckout, openWorkspace: vi.fn(), openActivity: vi.fn(),
    openChanges: vi.fn(),
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(<QueryClientProvider client={client}><ProjectTasksWorkspace {...props} /></QueryClientProvider>);
}

type User = ReturnType<typeof userEvent.setup>;
async function openDescription(user: User, item: Work) {
  await user.selectOptions(screen.getByRole("combobox", { name: "Kanban level" }), item === epic ? "epic" : item === subtask ? "subtask" : "task");
  await user.click(await screen.findByRole("button", { name: item.title }));
  const details = within(screen.getByRole("region", { name: item.title }));
  if (item !== epic) await user.click(details.getByRole("button", { name: "Context" }));
  await user.click(details.getByRole("button", { name: "Open description" }));
  const panel = within(await details.findByRole("region", { name: "Work description" }));
  const editorLabel = `Markdown editor for ${item.id}.md`;
  const editable = await panel.findByRole("textbox", { name: editorLabel });
  const editor = EditorView.findFromDOM(editable.closest<HTMLElement>(".cm-editor")!)!;
  const readingLabel = `Reading view for ${item.id}.md`;
  return { details, panel, editor, editorLabel, readingLabel };
}

function editSource(editor: EditorView, content: string) {
  act(() => editor.dispatch({ changes: { from: 0, to: editor.state.doc.length, insert: content } }));
}

it.each([epic, task, subtask])("edits and saves $title through Source, Live Preview and the shared Reading split", async (item) => {
  const states = descriptionFixture();
  const savedBefore = states.get(item.id)!.saved;
  const owner = { projectId, owner: item.owner };
  mountWorkspace();
  const user = userEvent.setup();
  const { details, panel, editor, editorLabel, readingLabel } = await openDescription(user, item);
  expect(invoke).toHaveBeenCalledWith("project_description_editor", owner);
  expect(invoke).toHaveBeenCalledWith("repository_note_catalog", { checkoutId });
  expect(editor.state.doc.toString()).toBe(savedBefore?.markdown ?? "");

  const expectChecklistUnchanged = async () => {
    if (item === epic) return;
    await user.click(details.getByRole("button", { name: "Overview" }));
    const checklist = within(details.getByRole("region", { name: "Task checklist" }));
    await waitFor(() => expect(checklist.getAllByRole<HTMLInputElement>("checkbox").map(({ checked }) => checked)).toEqual([false, true]));
    await user.click(details.getByRole("button", { name: "Context" }));
  };
  await expectChecklistUnchanged();

  await user.click(panel.getByRole("button", { name: "Source" }));
  const source = `# ${item.title}\n\nSource edit for this description.\n\n- [ ] Confirm description details\n`;
  editSource(editor, source);
  await waitFor(() => expect(states.get(item.id)!.draft?.markdown).toBe(source));
  expect(invoke).toHaveBeenCalledWith("save_project_description_draft", {
    ...owner, baseRevision: savedBefore?.id ?? null, expectedDraftRevision: null, markdown: source,
  });

  await user.click(panel.getByRole("button", { name: "Live Preview" }));
  const noteCheckbox = await panel.findByRole<HTMLInputElement>("checkbox", { name: "Toggle note task" });
  expect(noteCheckbox.checked).toBe(false);
  fireEvent.click(noteCheckbox);
  const final = source.replace("[ ]", "[x]");
  expect(editor.state.doc.toString()).toBe(final);
  await waitFor(() => expect(states.get(item.id)!.draft?.markdown).toBe(final));
  await expectChecklistUnchanged();
  expect(invoke).not.toHaveBeenCalledWith("revise_project_checklist_item", expect.anything());

  await user.click(panel.getByRole("button", { name: "Reading" }));
  let reading = within(await panel.findByLabelText(readingLabel, undefined, { timeout: 5000 }));
  expect(reading.getByText("Source edit for this description.")).toBeTruthy();
  expect(reading.getByRole<HTMLInputElement>("checkbox").checked).toBe(true);
  expect(panel.queryByRole("textbox", { name: editorLabel })).toBeNull();
  await user.click(panel.getByRole("button", { name: "Split Reading view" }));
  const visibleEditor = panel.getByRole("textbox", { name: editorLabel });
  expect(EditorView.findFromDOM(visibleEditor.closest<HTMLElement>(".cm-editor")!)).toBe(editor);
  reading = within(panel.getByLabelText(readingLabel));
  expect(reading.getByRole<HTMLInputElement>("checkbox").checked).toBe(true);

  const draft = states.get(item.id)!.draft!;
  await user.click(panel.getByRole("button", { name: "Save description" }));
  await waitFor(() => expect(states.get(item.id)!.saved?.markdown).toBe(final));
  expect(invoke).toHaveBeenCalledWith("save_project_description_editor", {
    ...owner, expectedRevision: savedBefore?.id ?? null, expectedDraftRevision: draft.revision, markdown: final,
  });
  await waitFor(() => expect(panel.queryByText("Unsaved changes")).toBeNull());
  await expectChecklistUnchanged();
});

it("restores a persisted draft after remount and rebases it only after the saved revision conflicts", async () => {
  const states = descriptionFixture();
  const state = states.get(subtask.id)!;
  const base = state.saved!;
  const owner = { projectId, owner: subtask.owner };
  const original = mountWorkspace();
  const user = userEvent.setup();
  const first = await openDescription(user, subtask);
  await user.click(first.panel.getByRole("button", { name: "Source" }));
  const local = "# Draft change\n\nKeep the unsaved notes.\n";
  editSource(first.editor, local);
  await waitFor(() => expect(state.draft?.markdown).toBe(local));
  const draft = state.draft!;
  original.unmount();

  mountWorkspace();
  const restored = await openDescription(user, subtask);
  expect(restored.editor.state.doc.toString()).toBe(local);
  expect(restored.panel.getByText("Unsaved changes")).toBeTruthy();
  const external = { id: "external-rev", revision: 2, markdown: "# External edit\n" };
  state.saved = external;
  state.history.set(external.id, external);

  await user.click(restored.panel.getByRole("button", { name: "Save description" }));
  expect(await restored.panel.findByText("The saved description changed.")).toBeTruthy();
  expect(restored.panel.getByText("# External edit", { exact: false })).toBeTruthy();
  expect(restored.editor.state.doc.toString()).toBe(local);
  expect(state.saved).toBe(external);
  expect(state.draft).toBe(draft);
  expect(invoke).toHaveBeenCalledWith("save_project_description_editor", {
    ...owner, expectedRevision: base.id, expectedDraftRevision: draft.revision, markdown: local,
  });

  await user.click(restored.panel.getByRole("button", { name: "Keep my edit" }));
  await waitFor(() => expect(state.draft?.base).toBe(external));
  const rebased = state.draft!;
  expect(rebased.revision).not.toBe(draft.revision);
  expect(invoke).toHaveBeenCalledWith("save_project_description_draft", {
    ...owner, baseRevision: external.id, expectedDraftRevision: draft.revision, markdown: local,
  });
  await user.click(restored.panel.getByRole("button", { name: "Save description" }));
  await waitFor(() => expect(state.saved?.markdown).toBe(local));
  expect(invoke).toHaveBeenCalledWith("save_project_description_editor", {
    ...owner, expectedRevision: external.id, expectedDraftRevision: rebased.revision, markdown: local,
  });
  expect(state.draft).toBeNull();
});
