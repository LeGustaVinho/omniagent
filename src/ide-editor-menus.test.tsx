// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { CodeEditor, type EditorSettings } from "@/ide-code-editor";
import { EditorStatus } from "@/ide-editor-status";
import { loadMonaco } from "@/ide-monaco";
import { IdeProblems, type FileProblem } from "@/ide-problems";

vi.mock("@/ide-monaco", () => ({ loadMonaco: vi.fn(), monacoTheme: () => "test" }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("shows Quick Fix only for supported diagnostics and keeps actions on the selected record", async () => {
  const issues: FileProblem[] = [
    { path: "src/app.ts", line: 9, column: 6, label: "Error", message: "Missing name", quickFix: true },
    { path: "src/app.ts", line: 11, column: 2, label: "Warning", message: "Unused value" },
  ];
  const open = vi.fn();
  const quickFix = vi.fn();
  const copy = vi.fn();
  const onContext = vi.fn();
  render(<IdeProblems issues={issues} open={open} reveal={vi.fn()} quickFix={quickFix} copy={copy} onContext={onContext} simulated={false} />);
  const user = userEvent.setup();
  fireEvent.contextMenu(screen.getByText(/Missing name/));
  await user.click(screen.getByRole("menuitem", { name: "Quick Fix…" }));
  expect(quickFix).toHaveBeenCalledWith(issues[0]);
  fireEvent.contextMenu(screen.getByText(/Unused value/));
  expect(screen.queryByRole("menuitem", { name: "Quick Fix…" })).toBeNull();
  await user.click(screen.getByRole("menuitem", { name: "Copy diagnostics in this file" }));
  expect(copy).toHaveBeenCalledWith("src/app.ts:9:6 Error: Missing name\nsrc/app.ts:11:2 Warning: Unused value", "Copied file diagnostics");
  fireEvent.contextMenu(screen.getByText(/Unused value/));
  await user.click(screen.getByRole("menuitem", { name: "Fix diagnostic…" }));
  expect(onContext).toHaveBeenCalledWith(issues[1], expect.stringContaining("fix the problem"));
});

it("positions and focuses Monaco before invoking the available native Quick Fix", async () => {
  const subscribe = () => ({ dispose: vi.fn() });
  const model = { getLanguageId: () => "typescript", onDidChangeContent: subscribe, onDidChangeLanguage: subscribe };
  const run = vi.fn(async () => undefined);
  const getAction = vi.fn(() => ({ isSupported: () => true, run }));
  const instance = { getAction, setPosition: vi.fn(), revealLineInCenter: vi.fn(), focus: vi.fn(), saveViewState: () => null, dispose: vi.fn() };
  vi.mocked(loadMonaco).mockResolvedValue({
    Uri: { from: () => ({ toString: () => "source-uri" }) },
    MarkerSeverity: { Error: 8, Warning: 4 },
    editor: { createModel: () => model, create: () => instance, getModelMarkers: () => [{ message: "Missing name", startLineNumber: 9, startColumn: 6, severity: 8 }], onDidChangeMarkers: subscribe },
  } as unknown as Awaited<ReturnType<typeof loadMonaco>>);
  const problems = vi.fn();
  const complete = vi.fn();
  render(<CodeEditor scope="test" path="src/app.ts" content="source" models={new Map()} update={vi.fn()} problems={problems}
    quickFix={{ line: 9, column: 6 }} onQuickFixShown={complete} />);
  await waitFor(() => expect(run).toHaveBeenCalledTimes(1));
  expect(getAction).toHaveBeenCalledWith("editor.action.quickFix");
  expect(instance.setPosition).toHaveBeenCalledWith({ lineNumber: 9, column: 6 });
  expect(instance.focus.mock.invocationCallOrder[0]).toBeLessThan(run.mock.invocationCallOrder[0]!);
  expect(problems).toHaveBeenCalledWith("src/app.ts", [{ label: "Error", message: "Missing name", line: 9, column: 6, quickFix: true }]);
  expect(complete).toHaveBeenCalledTimes(1);
});

it("offers supported model settings and copies the saved byte format reported by the repository", async () => {
  const settings: EditorSettings = { language: "typescript", autoLanguage: "typescript", tabSize: 2, insertSpaces: true,
    languages: [{ id: "typescript", label: "TypeScript" }, { id: "plaintext", label: "Plain Text" }],
    setLanguage: vi.fn(), setIndentation: vi.fn(), detectIndentation: vi.fn() };
  const copy = vi.fn();
  render(<EditorStatus path="src/app.ts" saved settings={settings} format={{ utf8Bom: true, lineEndings: "crlf" }} copy={copy} />);
  const user = userEvent.setup();
  fireEvent.contextMenu(screen.getByText("TypeScript"));
  await user.click(screen.getByRole("menuitem", { name: "Plain Text" }));
  expect(settings.setLanguage).toHaveBeenCalledWith("plaintext");
  fireEvent.contextMenu(screen.getByText("Spaces: 2"));
  await user.click(screen.getByRole("menuitem", { name: "Indent using tabs (4 columns)" }));
  expect(settings.setIndentation).toHaveBeenCalledWith(4, false);
  fireEvent.contextMenu(screen.getByText("UTF-8 with BOM · CRLF"));
  await user.click(screen.getByRole("menuitem", { name: "Copy saved file format" }));
  expect(copy).toHaveBeenCalledWith("src/app.ts · UTF-8 with BOM · CRLF", "Copied saved file format");
});

it("looks up the right-clicked source line in the editor's exact checkout and uses its unsaved text", async () => {
  const subscribe = () => ({ dispose: vi.fn() });
  let context: (event: { target: { position?: { lineNumber: number } } }) => void = () => undefined;
  let showBlame: (() => void) | undefined;
  const editorText = "first\nunsaved editor line\n";
  const getValue = vi.fn(() => editorText);
  const model = { getLanguageId: () => "typescript", getValue, onDidChangeContent: subscribe, onDidChangeLanguage: subscribe };
  const instance = {
    getPosition: () => ({ lineNumber: 1 }),
    addAction: ({ id, run }: { id: string; run: () => void }) => { if (id === "omniagent.git-line-blame") showBlame = run; },
    onContextMenu: (listener: typeof context) => { context = listener; return subscribe(); },
    onMouseDown: subscribe, onKeyDown: subscribe, getAction: () => undefined,
    focus: vi.fn(), saveViewState: () => null, dispose: vi.fn(),
  };
  vi.mocked(loadMonaco).mockResolvedValue({
    Uri: { from: () => ({ toString: () => "source-uri" }) },
    editor: { EndOfLinePreference: { LF: 1 }, createModel: () => model, create: () => instance, getModelMarkers: () => [], onDidChangeMarkers: subscribe },
  } as unknown as Awaited<ReturnType<typeof loadMonaco>>);
  vi.mocked(invoke).mockResolvedValueOnce({ status: "uncommitted", reason: "This editor line has no matching line in the checkout HEAD." });
  const copy = vi.fn();
  render(<CodeEditor scope="test" path="src/app.ts" content="saved" models={new Map()} update={vi.fn()} problems={vi.fn()}
    blameCheckoutId="linked-checkout" onCopy={copy} />);
  await waitFor(() => expect(showBlame).toBeTypeOf("function"));
  act(() => { context({ target: { position: { lineNumber: 2 } } }); showBlame?.(); });
  expect(await screen.findByText(/no matching line/)).toBeTruthy();
  expect(invoke).toHaveBeenCalledExactlyOnceWith("git_checkout_line_blame", { checkoutId: "linked-checkout", path: "src/app.ts", line: 2, editorText });
  expect(getValue).toHaveBeenCalledWith(1);
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Close dialog" }));
  const commit = "f123".repeat(10);
  vi.mocked(invoke).mockResolvedValueOnce({ status: "committed", commit, head: "00ff".repeat(10), author: "Author Name",
    summary: "First line", authoredAt: 123_456, headLine: 1, originalLine: 5 });
  act(() => showBlame?.());
  expect(await screen.findByText("Author Name")).toBeTruthy();
  expect(screen.getByText("src/app.ts:1")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Copy commit ID" }));
  expect(copy).toHaveBeenCalledWith(commit, "Copied commit ID");
});
