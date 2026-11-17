// @vitest-environment jsdom
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
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
};
type TerminalChannel = { onmessage: (event: { kind: "output"; data: string } | { kind: "exited"; exitCode: number }) => void };

const fixture = vi.hoisted(() => ({
  terminals: [] as FixtureTerminal[],
  dimensions: { cols: 96, rows: 25 },
  onResize: [] as (() => void)[],
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
    open = vi.fn();
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

beforeEach(() => {
  fixture.terminals.length = 0;
  fixture.onResize.length = 0;
  fixture.dimensions = { cols: 96, rows: 25 };
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(768);
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(420);
  vi.stubGlobal("ResizeObserver", class {
    constructor(callback: () => void) { fixture.onResize.push(callback); }
    observe() {}
    disconnect() {}
  });
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
