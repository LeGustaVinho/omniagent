// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { Uri, editor } from "monaco-editor";
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
  const disposeOpener = vi.fn();
  const registerEditorOpener = vi.fn<(opener: editor.ICodeEditorOpener) => { dispose: () => void }>()
    .mockReturnValue({ dispose: disposeOpener });
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
    Uri: { from: (parts: { scheme: string; path: string }) => ({ ...parts, toString: () => `${parts.scheme}:${parts.path}` }) },
    editor: {
      create: () => instance, createModel: () => model, registerEditorOpener,
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
  return { actions, commands, instance, clickTarget, selectThrough, capability, registerEditorOpener, disposeOpener };
}

it("dispatches only exact cross-file source URIs from its own Monaco editor", async () => {
  const monaco = sourceEditor();
  const onOpenLocation = vi.fn().mockResolvedValueOnce(true).mockResolvedValueOnce(false);
  const { unmount } = render(<CodeEditor scope="checkout:real" path="src/main.ts" content="source" models={new Map()}
    update={vi.fn()} problems={vi.fn()} onOpenLocation={onOpenLocation} />);
  await act(async () => {});
  const opener = monaco.registerEditorOpener.mock.lastCall![0];
  const editorSource = monaco.instance as unknown as editor.ICodeEditor;
  const uri = (path: string, extra: Partial<Uri> = {}) => ({ scheme: "omni-source", path, authority: "", query: "", fragment: "", ...extra }) as Uri;
  const target = "/checkout:real/src/ação %2F #1.ts";
  for (const invalid of [
    uri("/checkout:real/src/main.ts"), uri("/checkout-copy:real/src/main.ts"), uri("/checkout:simulation/src/main.ts"),
    uri("/checkout:real/../other.ts"), uri("/checkout:real/.git/config"), uri("/checkout:real/C:/other.ts"),
    uri("/checkout:real//src/other.ts"), uri("/checkout:real/src\\other.ts"), uri("/checkout:real/src. /other.ts"),
    uri(target, { scheme: "file" }), uri(target, { authority: "another-checkout" }), uri(target, { fragment: "wrong" }),
    uri(target, { query: "wrong" }),
  ]) expect(await opener.openCodeEditor(editorSource, invalid)).toBe(false);
  expect(await opener.openCodeEditor({} as editor.ICodeEditor, uri(target))).toBe(false);
  expect(await opener.openCodeEditor(editorSource, uri(target), { lineNumber: 0, column: 1 })).toBe(false);
  expect(onOpenLocation).not.toHaveBeenCalled();

  expect(await opener.openCodeEditor(editorSource, uri(target), { startLineNumber: 8, startColumn: 4, endLineNumber: 8, endColumn: 9 })).toBe(true);
  expect(onOpenLocation).toHaveBeenNthCalledWith(1, "src/ação %2F #1.ts", { line: 8, column: 4 });
  expect(await opener.openCodeEditor(editorSource, uri("/checkout:real/src/obsolete.ts"), { lineNumber: 2, column: 1 })).toBe(false);
  expect(onOpenLocation).toHaveBeenNthCalledWith(2, "src/obsolete.ts", { line: 2, column: 1 });
  unmount();
  expect(monaco.disposeOpener).toHaveBeenCalledOnce();
  expect(await opener.openCodeEditor(editorSource, uri(target))).toBe(false);
  expect(onOpenLocation).toHaveBeenCalledTimes(2);
});

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
