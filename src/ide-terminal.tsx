import { useEffect, useRef, useState } from "react";
import { Channel, invoke } from "@tauri-apps/api/core";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

const responses: Record<string, string[]> = {
  "$pwd": ["C:\\Example\\worktrees\\release-readiness"],
  "pwd": ["C:\\Example\\worktrees\\release-readiness"],
  "get-location": ["C:\\Example\\worktrees\\release-readiness"],
  "get-childitem": ["docs     src     tests     package.json"],
  "ls": ["docs     src     tests     package.json"],
  "git status": ["On branch release-readiness", " M src/health.ts", "?? tests/health.test.ts"],
  "pnpm test": [" ✓ tests/health.test.ts", " Test Files  1 passed", " Tests       1 passed"],
  "help": ["Example commands: Get-Location, Get-ChildItem, git status, pnpm test, Clear-Host"],
};

export function ExampleTerminal({ clearRevision }: { clearRevision: number }) {
  const mount = useRef<HTMLDivElement>(null);
  const instance = useRef<Terminal>(undefined);
  const previousClear = useRef(clearRevision);
  useEffect(() => {
    const element = mount.current;
    if (!element) return;
    const { terminal, fit } = openTerminal(element, 300);
    instance.current = terminal;
    const resize = new ResizeObserver(() => {
      if (element.clientWidth && element.clientHeight) fit.fit();
    });
    resize.observe(element);
    terminal.writeln("PowerShell · simulated worktree");
    terminal.writeln("Type help to see example commands.");
    const prompt = () => terminal.write("\x1b[36mPS release-readiness>\x1b[0m ");
    prompt();
    let buffer = "";
    const input = terminal.onData((data) => {
      if (data.startsWith("\x1b")) return;
      for (const character of data) {
        if (character === "\r" || character === "\n") {
          const command = buffer.trim().toLowerCase();
          buffer = "";
          terminal.write("\r\n");
          if (command === "cls" || command === "clear-host") terminal.clear();
          else if (command) for (const line of responses[command] ?? ["This command is unavailable in the example terminal. Try help."]) terminal.writeln(line);
          prompt();
        } else if (character === "\x03") {
          buffer = "";
          terminal.write("^C\r\n");
          prompt();
        } else if (character === "\x7f" && buffer) {
          buffer = buffer.slice(0, -1);
          terminal.write("\b \b");
        } else if (character >= " " && character !== "\x7f" && buffer.length < 500) {
          buffer += character;
          terminal.write(character);
        }
      }
    });
    return () => {
      resize.disconnect();
      input.dispose();
      terminal.dispose();
      instance.current = undefined;
    };
  }, []);
  useEffect(() => {
    if (previousClear.current !== clearRevision) {
      instance.current?.clear();
      previousClear.current = clearRevision;
    }
  }, [clearRevision]);
  return <div ref={mount} className="ide-terminal-surface" aria-label="Simulated PowerShell terminal" />;
}

function openTerminal(element: HTMLElement, scrollback: number) {
  const light = getComputedStyle(document.documentElement).colorScheme === "light";
  const terminal = new Terminal({
    cursorBlink: false,
    fontFamily: '"Geist Mono", Consolas, monospace',
    fontSize: 12,
    rows: 8,
    scrollback,
    screenReaderMode: true,
    theme: { background: light ? "#f7f7f9" : "#191919", foreground: light ? "#242428" : "#e7e7ec", cursor: light ? "#4a42b8" : "#a7a3ff" },
  });
  const fit = new FitAddon();
  terminal.loadAddon(fit);
  terminal.open(element);
  return { terminal, fit };
}

type Session = { checkoutId: string; terminalId: string };
type TerminalEvent = { kind: "output"; data: string }
  | { kind: "exited"; exitCode: number }
  | { kind: "error"; message: string };
type TerminalRequest = { checkoutId: string };
type TerminalState = {
  request: TerminalRequest;
  status: "starting" | "running" | "closing" | "exited" | "failed";
  message?: string;
};
type TerminalAttachment = { terminal: Terminal; layout: (focus: boolean) => void; close: () => void };

export function ManualTerminal({ checkoutId, active, clearRevision = 0 }: {
  checkoutId: string;
  active: boolean;
  clearRevision?: number;
}) {
  const mount = useRef<HTMLDivElement>(null);
  const attachment = useRef<TerminalAttachment>(undefined);
  const activeRef = useRef(active);
  const previousClear = useRef(clearRevision);
  const [request, setRequest] = useState<TerminalRequest>();
  const [state, setState] = useState<TerminalState>();
  const current = state?.request === request && request?.checkoutId === checkoutId ? state : undefined;

  useEffect(() => {
    const element = mount.current;
    if (!element || request?.checkoutId !== checkoutId) return;
    const { terminal, fit } = openTerminal(element, 1000);
    let session: Session | undefined;
    let disposed = false;
    let stopped = false;
    let exited = false;
    let failed = false;
    let sending = Promise.resolve<unknown>(undefined);
    const waitingInput: string[] = [];
    const set = (status: TerminalState["status"], message?: string) => {
      if (!disposed) setState({ request, status, message });
    };
    const close = () => {
      if (disposed || stopped || exited || !session) return;
      stopped = true;
      set("closing");
      void invoke("close_manual_terminal", session).catch((error: unknown) => {
        if (exited) return;
        stopped = false;
        set("running", String(error));
      });
    };
    const queue = (operation: (activeSession: Session) => Promise<unknown>) => {
      sending = sending.then(() => {
        if (!disposed && !stopped && !exited && session) return operation(session);
      }).catch((error: unknown) => {
        if (!stopped && !exited) set("running", String(error));
      });
    };
    const inputData = (data: string) => {
      if (disposed || stopped || exited) return;
      if (!session) waitingInput.push(data);
      else queue((activeSession) => invoke("write_manual_terminal", { ...activeSession, data }));
    };
    const input = terminal.onData(inputData);
    const channel = new Channel<TerminalEvent>((event) => {
      if (disposed) return;
      if (event.kind === "output") {
        terminal.write(Uint8Array.from(atob(event.data), (character) => character.charCodeAt(0)));
      } else if (event.kind === "error") {
        exited = true;
        failed = true;
        set("failed", event.message);
      } else {
        exited = true;
        if (!failed) set("exited", stopped || event.exitCode === 0 ? "PowerShell closed." : `PowerShell exited with code ${event.exitCode}.`);
      }
    });
    let size = { cols: terminal.cols, rows: terminal.rows };
    const layout = (focus: boolean) => {
      if (!activeRef.current || !element.clientWidth || !element.clientHeight) return;
      fit.fit();
      const next = { cols: terminal.cols, rows: terminal.rows };
      if (session && (next.cols !== size.cols || next.rows !== size.rows)) {
        size = next;
        queue((activeSession) => invoke("resize_manual_terminal", { ...activeSession, ...next }));
      }
      if (focus) terminal.focus();
    };
    attachment.current = { terminal, layout, close };

    layout(false);
    size = { cols: terminal.cols, rows: terminal.rows };
    const resize = new ResizeObserver(() => layout(false));
    resize.observe(element);
    void invoke<Session>("start_manual_terminal", { checkoutId, ...size, channel }).then((opened) => {
      if (disposed) {
        void invoke("close_manual_terminal", opened).catch(() => undefined);
        return;
      }
      session = opened;
      if (!exited) {
        set("running");
        // ConPTY may ask xterm for its cursor position before start returns its session ID.
        waitingInput.splice(0).forEach(inputData);
        layout(true);
      }
    }).catch((error: unknown) => {
      exited = true;
      set("failed", String(error));
    });
    return () => {
      disposed = true;
      resize.disconnect();
      input.dispose();
      terminal.dispose();
      attachment.current = undefined;
      if (session && !exited && !stopped) void invoke("close_manual_terminal", session).catch(() => undefined);
    };
  }, [checkoutId, request]);

  useEffect(() => {
    activeRef.current = active;
    if (active) attachment.current?.layout(true);
  }, [active]);

  useEffect(() => {
    if (previousClear.current !== clearRevision) {
      attachment.current?.terminal.clear();
      previousClear.current = clearRevision;
    }
  }, [clearRevision]);

  const start = () => {
    const next = { checkoutId };
    setRequest(next);
    setState({ request: next, status: "starting" });
  };
  return <div className="flex h-full min-h-0 flex-col gap-2">
    <div className="flex shrink-0 items-center gap-2">
      {(!current || current.status === "exited" || current.status === "failed") && (
        <button type="button" className="fw-btn fw-outlined" onClick={start}>Start PowerShell</button>
      )}
      {current?.status === "starting" && <span className="file-help" role="status">Starting PowerShell…</span>}
      {current?.status === "running" && (
        <button type="button" className="fw-btn fw-outlined" onClick={() => attachment.current?.close()}>Close PowerShell</button>
      )}
      {current?.status === "closing" && <span className="file-help" role="status">Closing PowerShell…</span>}
      {current?.message && <span className="file-help" role={current.status === "exited" ? "status" : "alert"}>{current.message}</span>}
    </div>
    <div ref={mount} className="ide-terminal-surface" aria-label="Manual PowerShell terminal" />
  </div>;
}
