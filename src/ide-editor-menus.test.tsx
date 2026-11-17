// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { CodeEditor, type EditorSettings } from "@/ide-code-editor";
import { EditorStatus } from "@/ide-editor-status";
import { loadMonaco } from "@/ide-monaco";
import { IdeProblems, type FileProblem } from "@/ide-problems";

vi.mock("@/ide-monaco", () => ({ loadMonaco: vi.fn(), monacoTheme: () => "test" }));
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
