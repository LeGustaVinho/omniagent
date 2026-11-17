// @vitest-environment jsdom
import { invoke } from "@tauri-apps/api/core";
import type { ILink, ILinkProvider } from "@xterm/xterm";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ManualTerminal } from "@/ide-terminal";

type Session = { checkoutId: string; terminalId: string };
type FixtureTerminal = {
  cols: number;
  rows: number;
  input?: (value: string) => void;
  write: ReturnType<typeof vi.fn>;
  focus: ReturnType<typeof vi.fn>;
  dispose: ReturnType<typeof vi.fn>;
  clear: ReturnType<typeof vi.fn>;
  paste: ReturnType<typeof vi.fn>;
  linkProvider?: ILinkProvider;
  selected: string;
  onOsc?: (data: string) => boolean;
  onScreenClear?: () => boolean;
  onTerminalResize?: () => void;
  onTerminalLineFeed?: () => void;
  onTerminalSelection?: () => void;
};
type TerminalChannel = { onmessage: (event: { kind: "output"; data: string } | { kind: "exited"; exitCode: number }) => void };

const fixture = vi.hoisted(() => ({
  terminals: [] as FixtureTerminal[],
  dimensions: { cols: 96, rows: 25 },
  onResize: [] as (() => void)[],
  findNext: vi.fn<(text: string) => boolean>(),
  lines: [] as { text: string; isWrapped: boolean }[],
  cursorRow: 0,
  cursorColumn: 0,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  Channel: class { constructor(public onmessage: TerminalChannel["onmessage"]) {} },
}));
vi.mock("@xterm/xterm", () => ({
  Terminal: class implements FixtureTerminal {
    cols = 80;
    rows = 8;
    input?: FixtureTerminal["input"];
    write = vi.fn();
    focus = vi.fn();
    dispose = vi.fn();
    clear = vi.fn();
    paste = vi.fn();
    element?: HTMLElement;
    selected = "";
    linkProvider?: ILinkProvider;
    open(element: HTMLElement) {
      this.element = element;
      const screen = document.createElement("div");
      screen.className = "xterm-screen";
      element.appendChild(screen);
    }
    registerLinkProvider(provider: ILinkProvider) {
      this.linkProvider = provider;
      return { dispose: () => { this.linkProvider = undefined; } };
    }
    getSelection() { return this.selected; }
    hasSelection() { return Boolean(this.selected); }
    getSelectionPosition() { return undefined; }
    private readonly oscHandlers: ((data: string) => boolean)[] = [];
    onOsc = (data: string) => this.oscHandlers.findLast((callback) => callback(data)) !== undefined;
    onScreenClear?: () => boolean;
    onTerminalResize?: () => void;
    onTerminalLineFeed?: () => void;
    onTerminalSelection?: () => void;
    parser = { registerOscHandler: (_id: number, handler: (data: string) => boolean) => {
      this.oscHandlers.push(handler);
      return { dispose: () => { this.oscHandlers.splice(this.oscHandlers.indexOf(handler), 1); } };
    }, registerCsiHandler: (id: { final: string }, handler: () => boolean) => {
      if (id.final === "J") this.onScreenClear = handler;
      return { dispose: () => { if (id.final === "J") this.onScreenClear = undefined; } };
    } };
    registerMarker() {
      let onDispose: (() => void) | undefined;
      return { line: fixture.cursorRow, onDispose: (listener: () => void) => { onDispose = listener; }, dispose: () => onDispose?.() };
    }
    attachCustomKeyEventHandler = vi.fn();
    onSelectionChange = (handler: () => void) => {
      this.onTerminalSelection = handler;
      return { dispose: () => { this.onTerminalSelection = undefined; } };
    };
    onResize = (handler: () => void) => {
      this.onTerminalResize = handler;
      return { dispose: () => { this.onTerminalResize = undefined; } };
    };
    onLineFeed = (handler: () => void) => {
      this.onTerminalLineFeed = handler;
      return { dispose: () => { this.onTerminalLineFeed = undefined; } };
    };
    get buffer() {
      return { onBufferChange: () => ({ dispose: vi.fn() }),
        active: { type: "normal", length: fixture.lines.length, viewportY: 0, baseY: 0, cursorY: fixture.cursorRow, cursorX: fixture.cursorColumn,
        getLine: (index: number) => {
        const line = fixture.lines[index];
        if (!line) return;
        const cells = Array.from(line.text).flatMap((character) => character === "界" ? [{ string: character, width: 2 }, { string: "", width: 0 }] : [{ string: character, width: 1 }]);
        return { length: cells.length, isWrapped: line.isWrapped,
          translateToString: (trim: boolean, start?: number, end?: number) => {
            const slice = line.text.slice(start, end);
            return trim ? slice.trimEnd() : slice;
          },
          getCell: (x: number) => cells[x] && { getChars: () => cells[x]!.string, getWidth: () => cells[x]!.width },
        };
      } } };
    }
    constructor() { fixture.terminals.push(this); }
    loadAddon(addon: { activate: (terminal: FixtureTerminal) => void }) { addon.activate(this); }
    onData(callback: (value: string) => void) {
      this.input = callback;
      return { dispose: () => { this.input = undefined; } };
    }
  },
}));
vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    terminal?: FixtureTerminal;
    activate(terminal: FixtureTerminal) { this.terminal = terminal; }
    fit() { if (this.terminal) Object.assign(this.terminal, fixture.dimensions); }
  },
}));
vi.mock("@xterm/addon-search", () => ({ SearchAddon: class {
  activate() {}
  findNext = fixture.findNext;
  findPrevious = vi.fn();
  clearDecorations = vi.fn();
} }));

beforeEach(() => {
  fixture.terminals.length = 0;
  fixture.onResize.length = 0;
  fixture.lines = [];
  fixture.cursorRow = 0;
  fixture.cursorColumn = 0;
  fixture.dimensions = { cols: 96, rows: 25 };
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(768);
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(420);
  vi.stubGlobal("ResizeObserver", class {
    constructor(callback: () => void) { fixture.onResize.push(callback); }
    observe() {}
    disconnect() {}
  });
});

it("opens reviewed input without a PTY and starts only after an explicit run with exact bytes", async () => {
  const reviewedInput = '$message = "café"\r\nWrite-Output $message\r\n';
  const session = { checkoutId: "reviewed-checkout", terminalId: "reviewed-terminal" };
  vi.mocked(invoke).mockResolvedValue(session);
  const user = userEvent.setup();
  render(<ManualTerminal checkoutId={session.checkoutId} active autoStart reviewedInput={reviewedInput} />);
  expect(screen.getByLabelText("Reviewed PowerShell input").textContent).toBe(reviewedInput);
  expect(invoke).not.toHaveBeenCalled();
  await user.click(screen.getByRole("button", { name: "Run reviewed input in PowerShell" }));
  await screen.findByRole("button", { name: "Close PowerShell" });
  expect(invoke).toHaveBeenCalledExactlyOnceWith("start_manual_terminal", expect.objectContaining({
    checkoutId: session.checkoutId, reviewedScript: reviewedInput,
  }));
  expect(invoke).not.toHaveBeenCalledWith("write_manual_terminal", expect.anything());
});

it("copies and attaches only the last authenticated command and its bounded rendered output", async () => {
  const session = { checkoutId: "checkout", terminalId: "session-token" };
  vi.mocked(invoke).mockResolvedValue(session);
  const add = vi.fn();
  const user = userEvent.setup();
  render(<ManualTerminal checkoutId={session.checkoutId} active autoStart onContextText={add} onOpenFile={vi.fn()} />);
  await screen.findByRole("button", { name: "Close PowerShell" });
  const terminal = fixture.terminals[0]!;
  const openMenu = () => fireEvent.contextMenu(screen.getByLabelText("Manual PowerShell terminal"));
  const start = (id: string, sequence: number, row = fixture.cursorRow, column = fixture.cursorColumn) => terminal.onOsc?.(
    `P;OmniAgentCommandStart=${id};${sequence};${row};${column}`);
  const end = (id: string, sequence: number, command: string, row = fixture.cursorRow, column = fixture.cursorColumn,
    status = "succeeded") => {
    const encoded = btoa(String.fromCharCode(...new TextEncoder().encode(command)));
    return terminal.onOsc?.(`P;OmniAgentCommandEnd=${id};${sequence};${row};${column};${status};${encoded}`);
  };
  fixture.lines = [
    { text: "PS> previously printed input", isWrapped: false },
    { text: "old> wrapping", isWrapped: false },
    { text: " text", isWrapped: true },
    { text: "okPS> new unexecuted input", isWrapped: false },
  ];
  act(() => {
    start("other-terminal", 1);
    end("other-terminal", 1, "untrusted command", fixture.cursorRow, fixture.cursorColumn, "failed");
  });
  openMenu();
  expect(screen.queryByRole("menuitem", { name: "Copy last PowerShell command" })).toBeNull();
  expect(screen.queryByRole("menuitem", { name: "Add last command failure to conversation…" })).toBeNull();
  await user.keyboard("{Escape}");

  fixture.cursorRow = 1;
  fixture.cursorColumn = 5;
  act(() => { start(session.terminalId, 1); });
  const command = "Write-Host 'é'";
  fixture.cursorRow = 3;
  fixture.cursorColumn = 2;
  act(() => { end(session.terminalId, 1, command); });
  // Later screen content cannot become part of the completed output.
  fixture.lines[3]!.text = "changed after command completion";
  openMenu();
  await user.click(await screen.findByRole("menuitem", { name: "Copy last PowerShell command" }));
  expect(await navigator.clipboard.readText()).toBe(command);
  openMenu();
  await user.click(await screen.findByRole("menuitem", { name: "Copy last command output" }));
  expect(await navigator.clipboard.readText()).toBe("wrapping text\nok");
  openMenu();
  expect(screen.queryByRole("menuitem", { name: "Add last command failure to conversation…" })).toBeNull();
  await user.click(await screen.findByRole("menuitem", { name: "Add last command output to conversation…" }));
  expect(add).toHaveBeenCalledExactlyOnceWith("wrapping text\nok");

  act(() => {
    start(session.terminalId, 2);
    terminal.onScreenClear?.();
    end(session.terminalId, 2, "Clear-Host");
  });
  openMenu();
  expect(screen.queryByRole("menuitem", { name: "Copy last command output" })).toBeNull();
  expect(screen.queryByRole("menuitem", { name: "Add last command output to conversation…" })).toBeNull();
  await user.click(await screen.findByRole("menuitem", { name: "Copy last PowerShell command" }));
  expect(await navigator.clipboard.readText()).toBe("Clear-Host");

  act(() => {
    start(session.terminalId, 3);
    end(session.terminalId, 3, "Get-Date", fixture.cursorRow + 1);
  });
  openMenu();
  expect(screen.queryByRole("menuitem", { name: "Copy last command output" })).toBeNull();
  await user.click(await screen.findByRole("menuitem", { name: "Copy last PowerShell command" }));
  expect(await navigator.clipboard.readText()).toBe("Get-Date");

  // ConPTY may send either receipt before it paints the corresponding row.
  fixture.lines[3]!.text = "fast output";
  fixture.lines.push({ text: "", isWrapped: false });
  fixture.cursorRow = 2;
  fixture.cursorColumn = 5;
  act(() => {
    start(session.terminalId, 4, 3, 0);
    end(session.terminalId, 4, "Write-Output 'fast output'", 4, 0);
  });
  openMenu();
  expect(screen.queryByRole("menuitem", { name: "Copy last command output" })).toBeNull();
  await user.keyboard("{Escape}");
  fixture.cursorRow = 3;
  fixture.cursorColumn = 0;
  act(() => terminal.onTerminalLineFeed?.());
  fixture.cursorRow = 4;
  act(() => terminal.onTerminalLineFeed?.());
  fixture.lines[4]!.text = "PS> next command";
  openMenu();
  await user.click(await screen.findByRole("menuitem", { name: "Copy last command output" }));
  expect(await navigator.clipboard.readText()).toBe("fast output\n");

  // Only receipts paired with an accepted line identify a command failure.
  act(() => end(session.terminalId, 0, "unmatched", fixture.cursorRow, fixture.cursorColumn, "failed"));
  openMenu();
  expect(screen.queryByRole("menuitem", { name: "Add last command failure to conversation…" })).toBeNull();
  await user.keyboard("{Escape}");

  const failed = "cmd.exe /d /c 'echo mixed output & exit 23'";
  fixture.lines = [{ text: "mixed output", isWrapped: false }, { text: "", isWrapped: false }];
  fixture.cursorRow = 0;
  act(() => { start(session.terminalId, 5); });
  fixture.cursorRow = 1;
  act(() => { end(session.terminalId, 5, failed, 1, 0, "failed"); });
  fixture.lines[1]!.text = "unrelated prompt";
  openMenu();
  await user.click(await screen.findByRole("menuitem", { name: "Add last command failure to conversation…" }));
  expect(add.mock.lastCall).toEqual([`PowerShell reported failure.\n\nCommand\n${failed}\n\nCaptured terminal output\nmixed output\n`]);
  terminal.selected = ".\\reported.ts:7";
  fireEvent.keyDown(screen.getByLabelText("Manual PowerShell terminal"), { key: "F10", shiftKey: true });
  expect(await screen.findByRole("menuitem", { name: "Open file at location" })).toBeTruthy();
  expect(await screen.findByRole("menuitem", { name: "Add last command failure to conversation…" })).toBeTruthy();
  await user.keyboard("{Escape}");

  const silent = "cmd.exe /d /c 'exit 23'";
  act(() => {
    start(session.terminalId, 6);
    terminal.onScreenClear?.();
    end(session.terminalId, 6, silent, 1, 0, "failed");
  });
  openMenu();
  await user.click(await screen.findByRole("menuitem", { name: "Add last command failure to conversation…" }));
  expect(add.mock.lastCall).toEqual([`PowerShell reported failure.\n\nCommand\n${silent}`]);

  act(() => { start(session.terminalId, 7); });
  openMenu();
  expect(screen.queryByRole("menuitem", { name: "Add last command failure to conversation…" })).toBeNull();
});

it("finds terminal output and passes one wrapped snapshot to the conversation chooser", async () => {
  vi.mocked(invoke).mockResolvedValue({ checkoutId: "checkout", terminalId: "terminal" });
  fixture.findNext.mockReturnValue(true);
  fixture.lines = [{ text: "Some ", isWrapped: false }, { text: "wrapped output  ", isWrapped: true }, { text: "", isWrapped: false }];
  const add = vi.fn();
  const user = userEvent.setup();
  render(<ManualTerminal checkoutId="checkout" active autoStart onContextText={add} />);
  await screen.findByRole("button", { name: "Close PowerShell" });
  const terminal = screen.getByLabelText("Manual PowerShell terminal");
  fireEvent.contextMenu(terminal);
  await user.click(await screen.findByRole("menuitem", { name: "Find in terminal" }));
  await user.type(screen.getByRole("searchbox", { name: "Find in terminal" }), "output");
  expect(fixture.findNext).toHaveBeenLastCalledWith("output", { incremental: true });
  await user.click(screen.getByRole("button", { name: "Close find" }));
  fireEvent.contextMenu(terminal);
  await user.click(await screen.findByRole("menuitem", { name: "Add terminal output to conversation…" }));
  expect(add).toHaveBeenCalledExactlyOnceWith("Some wrapped output");
  expect(invoke).not.toHaveBeenCalledWith("write_manual_terminal", expect.anything());
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.resetAllMocks(); vi.unstubAllGlobals(); });

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((settle) => { resolve = settle; });
  return { promise, resolve };
}

it("starts a manual checkout shell on request and preserves byte output, input order, size, focus, and close", async () => {
  const starting = deferred<Session>();
  const firstWrite = deferred<void>();
  const session = { checkoutId: "linked-checkout", terminalId: "terminal-1" };
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "start_manual_terminal") return starting.promise;
    if (command === "write_manual_terminal") return firstWrite.promise;
    return undefined;
  });
  const { rerender } = render(<ManualTerminal checkoutId={session.checkoutId} active />);
  expect(invoke).not.toHaveBeenCalled();
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Start PowerShell" }));
  expect(screen.getByRole("status").textContent).toBe("Starting PowerShell…");
  const start = vi.mocked(invoke).mock.calls[0]?.[1];
  expect(start).toMatchObject({ checkoutId: session.checkoutId, cols: 96, rows: 25 });
  const channel = (start as { channel: TerminalChannel }).channel;
  const terminal = fixture.terminals[0]!;
  terminal.input?.("\x1b[1;1R");
  expect(invoke).toHaveBeenCalledTimes(1);

  await act(async () => { starting.resolve(session); });
  await screen.findByRole("button", { name: "Close PowerShell" });
  expect(terminal.focus).toHaveBeenCalledTimes(1);
  const firstInput = { ...session, data: "\x1b[1;1R" };
  const secondInput = { ...session, data: "Get-Location\r" };
  terminal.input?.(secondInput.data);
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("write_manual_terminal", firstInput));
  expect(invoke).not.toHaveBeenCalledWith("write_manual_terminal", secondInput);
  await act(async () => { firstWrite.resolve(); });
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("write_manual_terminal", secondInput));

  channel.onmessage({ kind: "output", data: btoa("\xc3") });
  channel.onmessage({ kind: "output", data: btoa("\xa9") });
  expect(terminal.write).toHaveBeenNthCalledWith(1, Uint8Array.of(0xc3));
  expect(terminal.write).toHaveBeenNthCalledWith(2, Uint8Array.of(0xa9));

  const commands = vi.mocked(invoke).mock.calls.length;
  rerender(<ManualTerminal checkoutId={session.checkoutId} active clearRevision={1} />);
  expect(terminal.clear).toHaveBeenCalledTimes(1);
  expect(invoke).toHaveBeenCalledTimes(commands);

  rerender(<ManualTerminal checkoutId={session.checkoutId} active={false} clearRevision={1} />);
  fixture.dimensions = { cols: 110, rows: 32 };
  fixture.onResize[0]?.();
  expect(invoke).not.toHaveBeenCalledWith("close_manual_terminal", session);
  expect(invoke).not.toHaveBeenCalledWith("resize_manual_terminal", expect.anything());
  rerender(<ManualTerminal checkoutId={session.checkoutId} active clearRevision={1} />);
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("resize_manual_terminal", { ...session, cols: 110, rows: 32 }));
  expect(terminal.focus).toHaveBeenCalledTimes(2);

  await user.click(screen.getByRole("button", { name: "Close PowerShell" }));
  expect(invoke).toHaveBeenCalledWith("close_manual_terminal", session);
  act(() => channel.onmessage({ kind: "exited", exitCode: 1 }));
  expect(screen.getByRole("status").textContent).toBe("PowerShell closed.");
  expect(screen.getByRole("button", { name: "Start PowerShell" })).toBeTruthy();
});

it("closes a launch that returns after changing checkout and drops its late output", async () => {
  const starting = deferred<Session>();
  vi.mocked(invoke).mockImplementation(async (command) => command === "start_manual_terminal" ? starting.promise : undefined);
  const { rerender, unmount } = render(<ManualTerminal checkoutId="original" active />);
  await userEvent.setup().click(screen.getByRole("button", { name: "Start PowerShell" }));
  const original = fixture.terminals[0]!;
  const channel = (vi.mocked(invoke).mock.calls[0]?.[1] as { channel: TerminalChannel }).channel;
  rerender(<ManualTerminal checkoutId="replacement" active />);
  expect(original.dispose).toHaveBeenCalledTimes(1);
  expect(screen.getByRole("button", { name: "Start PowerShell" })).toBeTruthy();

  const obsolete = { checkoutId: "original", terminalId: "obsolete" };
  await act(async () => { starting.resolve(obsolete); });
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("close_manual_terminal", obsolete));
  channel.onmessage({ kind: "output", data: btoa("obsolete") });
  expect(original.write).not.toHaveBeenCalled();
  expect(invoke).toHaveBeenCalledTimes(2);
  unmount();
});

it("opens wrapped local file references with their location and offers a menu on the clicked cells", async () => {
  fixture.dimensions = { cols: 26, rows: 10 };
  const session = { checkoutId: "linked", terminalId: "session" };
  const original = "\".\\revisão #1.ts\":12:5";
  fixture.lines = [
    { text: "界 Error at (\".\\revisão #1", isWrapped: false },
    { text: ".ts\":12:5)", isWrapped: true },
    { text: "https://example.com/a.ts", isWrapped: false },
    { text: "C:\\check out\\a.ts:14:2", isWrapped: false },
  ];
  vi.mocked(invoke).mockImplementation(async (command) => command === "start_manual_terminal" ? session : { path: "docs/revisão #1.ts", kind: "file" });
  const onOpenFile = vi.fn();
  const onContextText = vi.fn();
  const split = vi.fn();
  const user = userEvent.setup();
  const { container } = render(<ManualTerminal checkoutId="linked" relativeDirectory="docs" active autoStart onOpenFile={onOpenFile}
    onContextText={onContextText} sessionActions={[[{ label: "Split terminal", run: split }]]} />);
  await screen.findByRole("button", { name: "Close PowerShell" });
  const terminal = fixture.terminals[0]!;
  let links: ILink[] | undefined;
  terminal.linkProvider?.provideLinks(2, (provided) => { links = provided; });
  expect(links).toHaveLength(1);
  expect(links![0]).toMatchObject({ text: original, range: { start: { x: 14, y: 1 }, end: { x: 9, y: 2 } } });
  links![0]!.activate(new MouseEvent("mouseup"), original);
  expect(onOpenFile).not.toHaveBeenCalled();
  await act(async () => { links![0]!.activate(new MouseEvent("mouseup", { ctrlKey: true }), original); });
  expect(invoke).toHaveBeenCalledWith("resolve_manual_terminal_path", { checkoutId: "linked", relativeDirectory: "docs", path: ".\\revisão #1.ts", expectedKind: "file" });
  expect(onOpenFile).toHaveBeenCalledExactlyOnceWith("docs/revisão #1.ts", 12, 5);
  terminal.linkProvider?.provideLinks(3, (provided) => { links = provided; });
  expect(links).toBeUndefined();
  terminal.linkProvider?.provideLinks(4, (provided) => { links = provided; });
  expect(links?.map(({ text }) => text)).toEqual(["C:\\check out\\a.ts:14:2"]);

  const grid = container.querySelector(".xterm-screen")!;
  vi.spyOn(grid, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, 260, 100));
  const openPathMenu = () => fireEvent.contextMenu(grid, { clientX: 30, clientY: 11 });
  openPathMenu();
  await user.click(await screen.findByRole("menuitem", { name: "Copy printed file reference" }));
  expect(await navigator.clipboard.readText()).toBe(original);

  terminal.selected = `Selected diagnostic at ${original}`;
  act(() => terminal.onTerminalSelection?.());
  openPathMenu();
  await user.click(await screen.findByRole("menuitem", { name: "Copy selection" }));
  expect(await navigator.clipboard.readText()).toBe(terminal.selected);
  openPathMenu();
  await user.click(await screen.findByRole("menuitem", { name: "Paste" }));
  expect(terminal.paste).toHaveBeenCalledExactlyOnceWith(terminal.selected);
  openPathMenu();
  await user.click(await screen.findByRole("menuitem", { name: "Add selection to conversation…" }));
  expect(onContextText).toHaveBeenCalledExactlyOnceWith(terminal.selected);
  openPathMenu();
  await user.click(await screen.findByRole("menuitem", { name: "Find in terminal" }));
  const find = await screen.findByRole("searchbox", { name: "Find in terminal" });
  await waitFor(() => expect(document.activeElement).toBe(find));
  await user.click(screen.getByRole("button", { name: "Close find" }));
  openPathMenu();
  await user.click(await screen.findByRole("menuitem", { name: "Split terminal" }));
  expect(split).toHaveBeenCalledTimes(1);

  terminal.selected = "../notes.md:8";
  fireEvent.keyDown(grid, { key: "F10", shiftKey: true });
  await user.click(await screen.findByRole("menuitem", { name: "Copy printed path" }));
  expect(await navigator.clipboard.readText()).toBe("../notes.md");
});

it("keeps an unverified reference and a late resolution from navigating", async () => {
  const pending = deferred<{ path: string; kind: "file" }>();
  fixture.lines = [{ text: "src/file.ts:1", isWrapped: false }];
  vi.mocked(invoke).mockImplementation(async (command) => {
    if (command === "start_manual_terminal") return { checkoutId: "checkout", terminalId: "terminal" };
    if (command === "resolve_manual_terminal_path") throw new Error("No file in this checkout.");
    return undefined;
  });
  const onOpenFile = vi.fn();
  const view = (active: boolean) => <ManualTerminal checkoutId="checkout" active={active} autoStart onOpenFile={onOpenFile} />;
  const { rerender } = render(view(true));
  await screen.findByRole("button", { name: "Close PowerShell" });
  const activate = () => fixture.terminals[0]?.linkProvider?.provideLinks(1, (links) => {
    links?.[0]?.activate(new MouseEvent("mouseup", { ctrlKey: true }), "src/file.ts:1");
  });
  act(activate);
  expect((await screen.findByRole("alert")).textContent).toContain("No file in this checkout.");
  expect(onOpenFile).not.toHaveBeenCalled();
  vi.mocked(invoke).mockImplementationOnce(async () => pending.promise);
  act(activate);
  rerender(view(false));
  rerender(view(true));
  await act(async () => { pending.resolve({ path: "src/file.ts", kind: "file" }); });
  expect(onOpenFile).not.toHaveBeenCalled();
});

it("reveals checked folders from printed terminal paths and uses the directory at the clicked row", async () => {
  const child = ".\\logs\\";
  const absolute = String.raw`C:\check out\logs folder`;
  fixture.lines = [
    { text: child, isWrapped: false },
    { text: absolute, isWrapped: false },
    { text: "Error: 'not a path'", isWrapped: false },
  ];
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "start_manual_terminal") return { checkoutId: "linked", terminalId: "terminal-token" };
    if (command === "resolve_manual_terminal_path") return { path: (args as { path: string }).path === child ? "old/logs" : "logs folder", kind: "folder" };
  });
  const onOpenFile = vi.fn();
  const onRevealFolder = vi.fn();
  const user = userEvent.setup();
  const { container } = render(<ManualTerminal checkoutId="linked" active autoStart onOpenFile={onOpenFile} onRevealFolder={onRevealFolder} />);
  await screen.findByRole("button", { name: "Close PowerShell" });
  const terminal = fixture.terminals[0]!;
  terminal.onOsc?.(`P;OmniAgentCwd=terminal-token;${btoa(String.raw`C:\check out\old`)}`);
  fixture.cursorRow = 1;
  terminal.onOsc?.(`P;OmniAgentCwd=terminal-token;${btoa(String.raw`C:\check out\new`)}`);

  const grid = container.querySelector(".xterm-screen")!;
  vi.spyOn(grid, "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, 960, 250));
  fireEvent.contextMenu(grid, { clientX: 12, clientY: 5 });
  await user.click(await screen.findByRole("menuitem", { name: "Reveal folder in Explorer tree" }));
  expect(invoke).toHaveBeenCalledWith("resolve_manual_terminal_path", {
    checkoutId: "linked", path: child, expectedKind: "folder", observedDirectory: String.raw`C:\check out\old`,
  });
  expect(onRevealFolder).toHaveBeenCalledExactlyOnceWith("old/logs");

  terminal.linkProvider?.provideLinks(2, (links) => expect(links?.map(({ text }) => text)).toEqual([absolute]));
  await act(async () => terminal.linkProvider?.provideLinks(2, (links) => links?.[0]?.activate(new MouseEvent("mouseup", { ctrlKey: true }), absolute)));
  expect(onRevealFolder).toHaveBeenNthCalledWith(2, "logs folder");
  expect(onOpenFile).not.toHaveBeenCalled();
  terminal.linkProvider?.provideLinks(3, (links) => expect(links).toBeUndefined());
});

it("keeps each relative file reference bound to its authenticated PowerShell prompt directory", async () => {
  fixture.lines = [{ text: "src/main.ts:1", isWrapped: false }, { text: "src/main.ts:2", isWrapped: false }];
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command === "start_manual_terminal") return { checkoutId: "checkout", terminalId: "session-token" };
    if (command === "resolve_manual_terminal_path") return { path: (args as { observedDirectory: string }).observedDirectory + "/src/main.ts", kind: "file" };
    return undefined;
  });
  const onOpenFile = vi.fn();
  render(<ManualTerminal checkoutId="checkout" active autoStart onOpenFile={onOpenFile} />);
  await screen.findByRole("button", { name: "Close PowerShell" });
  const terminal = fixture.terminals[0]!;
  const cwd = (token: string, path: string) => terminal.onOsc?.(`P;OmniAgentCwd=${token};${btoa(path)}`);
  cwd("session-token", String.raw`C:\checkout`);
  fixture.cursorRow = 1;
  cwd("session-token", String.raw`C:\checkout\subdirectory`);
  cwd("forged-token", String.raw`C:\unrelated`);
  for (const line of [1, 2]) await act(async () => terminal.linkProvider?.provideLinks(line, (links) => {
    links?.[0]?.activate(new MouseEvent("mouseup", { ctrlKey: true }), `src/main.ts:${line}`);
  }));
  expect(onOpenFile).toHaveBeenNthCalledWith(1, String.raw`C:\checkout/src/main.ts`, 1, undefined);
  expect(onOpenFile).toHaveBeenNthCalledWith(2, String.raw`C:\checkout\subdirectory/src/main.ts`, 2, undefined);
});
