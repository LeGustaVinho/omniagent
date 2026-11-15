import { useEffect, useRef } from "react";
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
    const light = getComputedStyle(document.documentElement).colorScheme === "light";
    const terminal = new Terminal({
      cursorBlink: false,
      fontFamily: '"Geist Mono", Consolas, monospace',
      fontSize: 12,
      rows: 8,
      scrollback: 300,
      screenReaderMode: true,
      theme: { background: light ? "#f7f7f9" : "#191919", foreground: light ? "#242428" : "#e7e7ec", cursor: light ? "#4a42b8" : "#a7a3ff" },
    });
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(element);
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
