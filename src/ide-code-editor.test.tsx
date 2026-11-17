// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { editor } from "monaco-editor";
import { afterEach, expect, it, vi } from "vitest";
import { CodeEditor } from "@/ide-code-editor";
import { loadMonaco } from "@/ide-monaco";

vi.mock("@/ide-monaco", () => ({ loadMonaco: vi.fn(), monacoTheme: () => "vs" }));
vi.mock("@/git-line-blame", () => ({
  GitLineBlameDialog: ({ request }: { request: { path: string; line: number } }) =>
    <div role="dialog" aria-label={`Blame ${request.path}:${request.line}`} />,
}));
afterEach(cleanup);

function sourceEditor(language = "typescript") {
  const source = ["selected.*text", "second line", "clicked third line"];
  let selection = { startLineNumber: 1, endLineNumber: 1, startColumn: 1, endColumn: 16, isEmpty: () => false };
  const actions = new Map<string, editor.IActionDescriptor>();
  const commands = new Map(["editor.action.copyLinesDownAction", "editor.action.duplicateSelection",
    "editor.action.commentLine", "actions.findWithSelection"].map((id) => [id, { isSupported: () => true, run: vi.fn() }]));
  const contextMenus: ((event: editor.IEditorMouseEvent) => void)[] = [];
  const selectionChanged = vi.fn<(callback: () => void) => { dispose: () => void }>().mockReturnValue({ dispose: vi.fn() });
  const capability = { set: vi.fn() };
  const model = {
    getLanguageId: () => language,
    getLineContent: (line: number) => source[line - 1],
    getValue: () => source.join("\n"),
    getValueInRange: ({ startLineNumber, endLineNumber }: { startLineNumber: number; endLineNumber: number }) =>
      source.slice(startLineNumber - 1, endLineNumber).join("\n"),
    onDidChangeContent: () => ({ dispose: vi.fn() }),
    onDidChangeLanguage: () => ({ dispose: vi.fn() }),
  };
  const instance = {
    getSelection: () => selection,
    getPosition: () => ({ lineNumber: 1, column: 2 }),
    getModel: () => model,
    onContextMenu: (handler: (event: editor.IEditorMouseEvent) => void) => {
      contextMenus.push(handler);
      return { dispose: vi.fn() };
    },
    onMouseDown: () => ({ dispose: vi.fn() }),
    onKeyDown: () => ({ dispose: vi.fn() }),
    onDidChangeCursorSelection: selectionChanged,
    createContextKey: () => capability,
    getAction: (id: string) => commands.get(id),
    addAction: (descriptor: editor.IActionDescriptor) => {
      actions.set(descriptor.id, descriptor);
      return { dispose: vi.fn() };
    },
    setPosition: vi.fn(),
    saveViewState: () => null,
    focus: vi.fn(),
    dispose: vi.fn(),
  };
  vi.mocked(loadMonaco).mockResolvedValue({
    Uri: { from: () => ({ toString: () => "omni-source://test" }) },
    editor: {
      create: () => instance, createModel: () => model,
      setTheme: vi.fn(), getModelMarkers: () => [], onDidChangeMarkers: () => ({ dispose: vi.fn() }),
      EndOfLinePreference: { LF: 1 }, MouseTargetType: { GUTTER_LINE_NUMBERS: 3 },
    },
  } as unknown as Awaited<ReturnType<typeof loadMonaco>>);
  function clickTarget(type: number, lineNumber: number) {
    const event = { preventDefault: vi.fn(), stopPropagation: vi.fn(), browserEvent: { clientX: 22, clientY: 40 } };
    act(() => contextMenus.forEach((open) => open({ event, target: { type, position: { lineNumber, column: 1 } } } as unknown as editor.IEditorMouseEvent)));
    return event;
  }
  function selectThrough(line: number) {
    selection = { ...selection, endLineNumber: line };
    act(() => selectionChanged.mock.calls[0]?.[0]());
  }
  return { actions, commands, instance, clickTarget, selectThrough, capability };
}

it("delegates edit and file find to Monaco and scopes project find to a supported literal selection", async () => {
  const monaco = sourceEditor();
  const onSearchText = vi.fn();
  render(<CodeEditor scope="checkout" path="src/main.ts" content="source" models={new Map()} update={vi.fn()} problems={vi.fn()} onSearchText={onSearchText} />);
  await act(async () => {});
  for (const [action, command] of [
    ["duplicate-line", "editor.action.copyLinesDownAction"],
    ["duplicate-selection", "editor.action.duplicateSelection"],
    ["toggle-line-comment", "editor.action.commentLine"],
    ["find-selection", "actions.findWithSelection"],
  ] as const) {
    await monaco.actions.get(`omniagent.${action}`)!.run(monaco.instance as unknown as editor.ICodeEditor);
    expect(monaco.commands.get(command)?.run).toHaveBeenCalledOnce();
  }
  const search = monaco.actions.get("omniagent.find-selection-in-project")!;
  expect(monaco.capability.set).toHaveBeenLastCalledWith(true);
  await search.run(monaco.instance as unknown as editor.ICodeEditor);
  expect(onSearchText).toHaveBeenCalledExactlyOnceWith("selected.*text");
  monaco.selectThrough(2);
  expect(monaco.capability.set).toHaveBeenLastCalledWith(false);
  await search.run(monaco.instance as unknown as editor.ICodeEditor);
  expect(onSearchText).toHaveBeenCalledTimes(1);
});

it("keeps the content menu native and cites, edits or blames the clicked gutter line", async () => {
  const monaco = sourceEditor();
  const onCopy = vi.fn();
  const onExcerpt = vi.fn();
  const onOmniAgentLink = vi.fn();
  render(<CodeEditor scope="checkout" path="src/main.ts" content="source" models={new Map()} update={vi.fn()} problems={vi.fn()}
    onCopy={onCopy} onExcerpt={onExcerpt} onOmniAgentLink={onOmniAgentLink} blameCheckoutId="checkout" />);
  await act(async () => {});
  expect(monaco.clickTarget(6, 1).preventDefault).not.toHaveBeenCalled();
  expect(screen.queryByRole("menu")).toBeNull();

  const user = userEvent.setup();
  const openLine = () => {
    expect(monaco.clickTarget(3, 3).preventDefault).toHaveBeenCalledOnce();
    return screen.getByRole("menu", { name: "src/main.ts:3" });
  };
  openLine();
  await user.click(screen.getByRole("menuitem", { name: "Copy line" }));
  expect(onCopy).toHaveBeenCalledWith("clicked third line", "Copied line");
  expect(monaco.instance.setPosition).not.toHaveBeenCalled();
  openLine();
  await user.click(screen.getByRole("menuitem", { name: "Copy OmniAgent link to line" }));
  expect(onOmniAgentLink).toHaveBeenCalledWith("src/main.ts", 3, 1);
  openLine();
  await user.click(screen.getByRole("menuitem", { name: "Add line to conversation…" }));
  expect(onExcerpt).toHaveBeenCalledWith({ path: "src/main.ts", startLine: 3, endLine: 3, text: "clicked third line", language: "typescript" });
  openLine();
  await user.click(screen.getByRole("menuitem", { name: "Duplicate line" }));
  expect(monaco.instance.setPosition).toHaveBeenCalledExactlyOnceWith({ lineNumber: 3, column: 1 });
  expect(monaco.commands.get("editor.action.copyLinesDownAction")?.run).toHaveBeenCalledOnce();
  openLine();
  await user.click(screen.getByRole("menuitem", { name: "Show Git blame for line…" }));
  expect(screen.getByRole("dialog", { name: "Blame src/main.ts:3" })).toBeTruthy();
});
