import { useCallback, useEffect, useRef, useState } from "react";
import { Channel, invoke } from "@tauri-apps/api/core";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { ContextActions, PositionedContextActions, type MenuGroups } from "@/components/ui/context-actions";
import { markdownExcerpt } from "@/editor-context";
import { trackTerminalCommands, type TerminalCommand } from "@/terminal-command-capture";
import { parseTerminalPathReference, terminalPathAtPoint, terminalPathLinkProvider, trackTerminalDirectories, type ResolvedTerminalPath, type TerminalPathReference } from "@/terminal-file-links";
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
type TerminalRequest = { checkoutId: string; reviewedScript?: string };
type TerminalState = {
  request: TerminalRequest;
  status: "starting" | "running" | "closing" | "exited" | "failed";
  message?: string;
};
type TerminalAttachment = { terminal: Terminal; search: SearchAddon; layout: (focus: boolean) => void; close: () => void; clear: () => void;
  directoryAt?: (line?: number) => string | undefined };

function terminalOutput(terminal: Terminal) {
  const buffer = terminal.buffer.active;
  const lines: string[] = [];
  for (let index = 0; index < buffer.length; index++) {
    const line = buffer.getLine(index);
    const nextWrap = buffer.getLine(index + 1)?.isWrapped;
    const text = line?.translateToString(!nextWrap) ?? "";
    if (line?.isWrapped && lines.length) lines[lines.length - 1] += text;
    else lines.push(text);
  }
  while (lines.at(-1) === "") lines.pop();
  return lines.join("\n");
}

function failedCommandContext({ command, output }: TerminalCommand) {
  return ["PowerShell reported failure.", `Command\n${command}`,
    output?.trim() ? `Captured terminal output\n${output}` : undefined].filter(Boolean).join("\n\n");
}

export function ManualTerminal({ checkoutId, relativeDirectory, active, visible = active, autoStart, clearRevision = 0,
  onContextText, onOpenFile, onRevealFolder, onFocus, sessionActions, reviewedInput }: {
  checkoutId: string;
  relativeDirectory?: string;
  active: boolean;
  visible?: boolean;
  autoStart?: boolean;
  clearRevision?: number;
  onContextText?: (text: string) => void;
  onOpenFile?: (path: string, line?: number, column?: number) => void;
  onRevealFolder?: (path: string) => void;
  onFocus?: () => void;
  sessionActions?: MenuGroups;
  reviewedInput?: string;
}) {
  const mount = useRef<HTMLDivElement>(null);
  const attachment = useRef<TerminalAttachment>(undefined);
  const activeRef = useRef(active);
  const visibleRef = useRef(visible);
  const previousClear = useRef(clearRevision);
  const openFile = useRef(onOpenFile);
  const revealFolder = useRef(onRevealFolder);
  const navigateSequence = useRef(0);
  const [request, setRequest] = useState<TerminalRequest>();
  const [state, setState] = useState<TerminalState>();
  const [lastCommand, setLastCommand] = useState<{ request: TerminalRequest; result: TerminalCommand }>();
  const [clipboardError, setClipboardError] = useState("");
  const [fileError, setFileError] = useState("");
  const [referenceMenu, setReferenceMenu] = useState<{ reference: TerminalPathReference; point: { x: number; y: number }; terminal: Terminal; directory?: string }>();
  const restoringTerminalFocus = useRef(true);
  const [hasSelection, setHasSelection] = useState(false);
  const [findOpen, setFindOpen] = useState(false);
  const [findText, setFindText] = useState("");
  const [found, setFound] = useState<boolean>();
  const findInput = useRef<HTMLInputElement>(null);
  const current = state?.request === request && request?.checkoutId === checkoutId ? state : undefined;
  const lastExecuted = current && lastCommand && lastCommand.request === request ? lastCommand.result : undefined;
  const lastOutput = lastExecuted?.output;

  useEffect(() => { openFile.current = onOpenFile; }, [onOpenFile]);
  useEffect(() => { revealFolder.current = onRevealFolder; }, [onRevealFolder]);
  const openReference = useCallback((terminal: Terminal, { path, line, column, expectedKind }: TerminalPathReference, directory?: string) => {
    if ((!openFile.current && !revealFolder.current) || attachment.current?.terminal !== terminal || !visibleRef.current) return;
    const sequence = ++navigateSequence.current;
    setFileError("");
    const isCurrent = () => sequence === navigateSequence.current && attachment.current?.terminal === terminal && visibleRef.current;
    void invoke<ResolvedTerminalPath>("resolve_manual_terminal_path", { checkoutId, path, expectedKind, ...(relativeDirectory ? { relativeDirectory } : {}),
      ...(directory !== undefined ? { observedDirectory: directory } : {}) })
      .then((resolved) => {
        if (!isCurrent()) return;
        if (resolved.kind === "folder") revealFolder.current?.(resolved.path);
        else openFile.current?.(resolved.path, line, column);
      })
      .catch((error: unknown) => { if (isCurrent()) setFileError(String(error)); });
  }, [checkoutId, relativeDirectory]);

  useEffect(() => {
    if (!request && autoStart && active && reviewedInput === undefined) {
      const next = { checkoutId };
      setRequest(next);
      setState({ request: next, status: "starting" });
    }
  }, [active, autoStart, checkoutId, request, reviewedInput]);

  useEffect(() => {
    const element = mount.current;
    if (!element || request?.checkoutId !== checkoutId) return;
    const { terminal, fit } = openTerminal(element, 1000);
    const search = new SearchAddon();
    terminal.loadAddon(search);
    let session: Session | undefined;
    const commands = trackTerminalCommands(terminal, () => session?.terminalId, (result) => {
      setLastCommand(result ? { request, result } : undefined);
    });
    const pathNavigation = openFile.current || revealFolder.current;
    const directories = pathNavigation && trackTerminalDirectories(terminal, () => session?.terminalId);
    const pathLinks = pathNavigation && terminal.registerLinkProvider(terminalPathLinkProvider(terminal,
      (reference) => openReference(terminal, reference, directories?.at(reference.bufferLine))));
    const selection = terminal.onSelectionChange(() => setHasSelection(terminal.hasSelection()));
    terminal.attachCustomKeyEventHandler((event) => {
      if (event.type !== "keydown" || !(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== "f") return true;
      event.preventDefault();
      setFindOpen(true);
      findInput.current?.focus();
      return false;
    });
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
      if (!visibleRef.current || !element.clientWidth || !element.clientHeight) return;
      fit.fit();
      const next = { cols: terminal.cols, rows: terminal.rows };
      if (session && (next.cols !== size.cols || next.rows !== size.rows)) {
        size = next;
        queue((activeSession) => invoke("resize_manual_terminal", { ...activeSession, ...next }));
      }
      if (focus && activeRef.current) terminal.focus();
    };
    const clear = () => { commands.discardOutputStart(); terminal.clear(); };
    attachment.current = { terminal, search, layout, close, clear, directoryAt: directories?.at };

    layout(false);
    size = { cols: terminal.cols, rows: terminal.rows };
    const resize = new ResizeObserver(() => layout(false));
    resize.observe(element);
    void invoke<Session>("start_manual_terminal", { checkoutId, ...size, channel, ...(relativeDirectory ? { relativeDirectory } : {}),
      ...(request.reviewedScript === undefined ? {} : { reviewedScript: request.reviewedScript }) }).then((opened) => {
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
      selection.dispose();
      commands.dispose();
      directories?.dispose();
      pathLinks?.dispose();
      input.dispose();
      terminal.dispose();
      attachment.current = undefined;
      if (session && !exited && !stopped) void invoke("close_manual_terminal", session).catch(() => undefined);
    };
  }, [checkoutId, openReference, request, relativeDirectory]);

  useEffect(() => {
    activeRef.current = active;
    visibleRef.current = visible;
    if (visible) attachment.current?.layout(active);
    else {
      navigateSequence.current++;
      setReferenceMenu(undefined);
    }
  }, [active, visible]);

  useEffect(() => {
    if (previousClear.current !== clearRevision) {
      attachment.current?.clear();
      previousClear.current = clearRevision;
    }
  }, [clearRevision]);

  const canStart = !current || current.status === "exited" || current.status === "failed";
  function start(reviewedScript?: string) {
    setHasSelection(false);
    setFileError("");
    setReferenceMenu(undefined);
    setFindOpen(false);
    setFound(undefined);
    const next = { checkoutId, reviewedScript };
    setRequest(next);
    setState({ request: next, status: "starting" });
  }
  function clipboard(operation: (terminal: Terminal) => Promise<unknown>) {
    const terminal = attachment.current?.terminal;
    if (!terminal) return;
    setClipboardError("");
    void Promise.resolve().then(() => operation(terminal)).catch(() => setClipboardError("Could not access the clipboard."));
  }
  function find(text: string, previous = false, incremental = false) {
    const search = attachment.current?.search;
    if (!search) return;
    if (!text) { search.clearDecorations(); setFound(undefined); return; }
    setFound(previous ? search.findPrevious(text, { incremental }) : search.findNext(text, { incremental }));
  }
  function dismissFind() {
    setFindOpen(false);
    setFound(undefined);
    attachment.current?.search.clearDecorations();
    attachment.current?.terminal.focus();
  }
  function showReferenceMenu(terminal: Terminal, reference: TerminalPathReference, x: number, y: number) {
    restoringTerminalFocus.current = true;
    const directory = attachment.current?.directoryAt?.(reference.bufferLine);
    setReferenceMenu({ terminal, reference, point: { x, y }, directory });
  }
  function addContext(text: string) {
    restoringTerminalFocus.current = false;
    onContextText?.(text);
  }
  const terminalActions: MenuGroups = [[
    { label: "Copy selection", disabled: !hasSelection, run: () => clipboard((terminal) => {
      const selection = terminal.getSelection();
      return selection ? navigator.clipboard.writeText(selection) : Promise.resolve();
    }) },
    { label: "Copy selection as Markdown", disabled: !hasSelection, run: () => clipboard((terminal) =>
      navigator.clipboard.writeText(markdownExcerpt({ text: terminal.getSelection() }))) },
    { label: "Copy terminal output", disabled: !request, run: () => clipboard((terminal) => navigator.clipboard.writeText(terminalOutput(terminal))) },
    { label: "Paste", disabled: current?.status !== "running", run: () => clipboard(async (terminal) => {
      const text = await navigator.clipboard.readText();
      if (attachment.current?.terminal === terminal) terminal.paste(text);
    }) },
    { label: "Select all", disabled: !request, run: () => attachment.current?.terminal.selectAll() },
  ], [
    { label: "Find in terminal", disabled: !request, run: () => {
      restoringTerminalFocus.current = false;
      setFindOpen(true);
      requestAnimationFrame(() => findInput.current?.focus());
    } },
    { label: "Scroll to top", disabled: !request, run: () => attachment.current?.terminal.scrollToTop() },
    { label: "Scroll to bottom", disabled: !request, run: () => attachment.current?.terminal.scrollToBottom() },
    { label: "Clear output", disabled: !request, run: () => attachment.current?.clear() },
  ], lastExecuted ? [
    { label: "Copy last PowerShell command", run: () => clipboard(() => navigator.clipboard.writeText(lastExecuted.command)) },
    ...(lastOutput ? [{ label: "Copy last command output", run: () => clipboard(() => navigator.clipboard.writeText(lastOutput)) }] : []),
    ...(onContextText && lastOutput?.trim() ? [{ label: "Add last command output to conversation…", run: () => addContext(lastOutput) }] : []),
    ...(onContextText && lastExecuted.failed ? [{ label: "Add last command failure to conversation…", run: () => addContext(failedCommandContext(lastExecuted)) }] : []),
  ] : [], onContextText ? [
    { label: "Add selection to conversation…", disabled: !hasSelection, run: () => {
      const text = attachment.current?.terminal.getSelection();
      if (text) addContext(text);
    } },
    { label: "Add terminal output to conversation…", disabled: !request, run: () => {
      const terminal = attachment.current?.terminal;
      if (terminal) addContext(terminalOutput(terminal));
    } },
  ] : [], [
    ...(canStart && reviewedInput === undefined ? [{ label: "Start PowerShell", run: () => start() }] : []),
    ...(current?.status === "running" ? [{ label: "Close PowerShell", run: () => attachment.current?.close() }] : []),
  ], ...(sessionActions ?? [])];
  return <div className="flex h-full min-h-0 flex-col gap-2" onFocusCapture={onFocus} onPointerDownCapture={onFocus}>
    <div className="flex shrink-0 items-center gap-2">
      {canStart && reviewedInput === undefined && (
        <button type="button" className="fw-btn fw-outlined" onClick={() => start()}>Start PowerShell</button>
      )}
      {current?.status === "starting" && <span className="file-help" role="status">Starting PowerShell…</span>}
      {current?.status === "running" && (
        <button type="button" className="fw-btn fw-outlined" onClick={() => attachment.current?.close()}>Close PowerShell</button>
      )}
      {current?.status === "closing" && <span className="file-help" role="status">Closing PowerShell…</span>}
      {current?.message && <span className="file-help" role={current.status === "exited" ? "status" : "alert"}>{current.message}</span>}
    </div>
    {canStart && reviewedInput !== undefined && <section className="grid min-h-0 shrink-0 gap-2" aria-label="Review PowerShell input">
      <pre className="max-h-36 overflow-auto whitespace-pre-wrap wrap-anywhere font-mono text-xs" tabIndex={0}
        aria-label="Reviewed PowerShell input">{reviewedInput}</pre>
      <button type="button" className="fw-btn fw-primary justify-self-start" onClick={() => start(reviewedInput)}>
        Run reviewed input in PowerShell
      </button>
    </section>}
    {clipboardError && <p className="file-help" role="alert">{clipboardError}</p>}
    {fileError && <p className="file-help" role="alert">{fileError}</p>}
    {findOpen && <form className="flex shrink-0 flex-wrap items-center gap-1" aria-label="Terminal search"
      onSubmit={(event) => { event.preventDefault(); find(findText); }}
      onKeyDown={(event) => {
        if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); dismissFind(); }
        if (event.key === "Enter" && event.shiftKey) { event.preventDefault(); find(findText, true); }
      }}>
      <input ref={findInput} autoFocus type="search" className="delivery-filter min-w-0 flex-1" aria-label="Find in terminal"
        value={findText} onChange={(event) => { setFindText(event.currentTarget.value); find(event.currentTarget.value, false, true); }} />
      <button type="button" className="fw-btn" disabled={!findText} onClick={() => find(findText, true)}>Previous</button>
      <button type="submit" className="fw-btn" disabled={!findText}>Next</button>
      <button type="button" className="fw-btn" onClick={dismissFind}>Close find</button>
      {found === false && <span role="status" className="file-help">No matching terminal output.</span>}
    </form>}
    <ContextActions label="PowerShell terminal" className="min-h-0 flex-1" groups={terminalActions}>
      <div ref={mount} className="ide-terminal-surface" aria-label="Manual PowerShell terminal" onContextMenu={(event) => {
        const terminal = attachment.current?.terminal;
        if ((!onOpenFile && !onRevealFolder) || !terminal) return;
        const reference = terminalPathAtPoint(terminal, event);
        if (!reference) return;
        event.preventDefault();
        event.stopPropagation();
        showReferenceMenu(terminal, reference, event.clientX, event.clientY);
      }} onKeyDownCapture={(event) => {
        if ((!onOpenFile && !onRevealFolder) || (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10"))) return;
        const terminal = attachment.current?.terminal;
        const reference = terminal && parseTerminalPathReference(terminal.getSelection().trim(), true);
        if (!terminal || !reference) return;
        event.preventDefault();
        event.stopPropagation();
        const bounds = event.currentTarget.getBoundingClientRect();
        const bufferLine = terminal.getSelectionPosition()?.start.y;
        showReferenceMenu(terminal, { ...reference, bufferLine }, bounds.left, bounds.top);
      }} />
    </ContextActions>
    {referenceMenu && visible && <PositionedContextActions label={referenceMenu.reference.text} point={referenceMenu.point}
      onClose={() => setReferenceMenu(undefined)} restoreFocus={() => {
        if (restoringTerminalFocus.current && attachment.current?.terminal === referenceMenu.terminal) referenceMenu.terminal.focus();
      }} groups={[[
        { label: referenceMenu.reference.expectedKind === "folder" ? "Reveal folder in Explorer tree"
          : referenceMenu.reference.line ? "Open file at location" : "Open checkout path", run: () => {
          restoringTerminalFocus.current = false;
          openReference(referenceMenu.terminal, referenceMenu.reference, referenceMenu.directory);
        } },
        ...(referenceMenu.reference.line || referenceMenu.reference.text !== referenceMenu.reference.path ? [{
          label: referenceMenu.reference.line ? "Copy printed file reference" : "Copy printed reference",
          run: () => clipboard(() => navigator.clipboard.writeText(referenceMenu.reference.text)),
        }] : []),
        { label: "Copy printed path", run: () => clipboard(() => navigator.clipboard.writeText(referenceMenu.reference.path)) },
      ], ...terminalActions]} />}
  </div>;
}
