import type { IMarker, Terminal } from "@xterm/xterm";

export type TerminalCommand = { command: string; failed: boolean; output?: string };
type OutputStart = { marker: IMarker; column: number };
type LinePosition = { row: number; baseY: number };
type PendingOutput = {
  sequence: number;
  start: OutputStart | LinePosition;
  completion?: LinePosition & { result: TerminalCommand };
};

const startPrefix = "P;OmniAgentCommandStart=";
const endPrefix = "P;OmniAgentCommandEnd=";

function readOutput(terminal: Terminal, start: OutputStart): string | undefined {
  const { active: buffer } = terminal.buffer;
  const first = start.marker.line;
  const last = buffer.baseY + buffer.cursorY;
  const lastColumn = buffer.cursorX;
  if (buffer.type !== "normal" || start.marker.isDisposed || first < 0 || first > last
    || (first === last && start.column > lastColumn)) return;
  const lines: string[] = [];
  for (let index = first; index <= last; index++) {
    const row = buffer.getLine(index);
    if (!row) return;
    const following = index < last ? buffer.getLine(index + 1) : undefined;
    const followingWraps = following?.isWrapped;
    const from = index === first ? start.column : 0;
    // A wide glyph at the next row's start can leave one unused cell before a wrap.
    const unusedWideCell = followingWraps && following?.getCell(0)?.getWidth() === 2
      && !row.getCell(terminal.cols - 1)?.getChars();
    const to = index === last ? lastColumn : unusedWideCell ? terminal.cols - 1 : undefined;
    const text = row.translateToString(index < last && !followingWraps, from, to);
    if (index !== first && row.isWrapped) lines[lines.length - 1] += text;
    else lines.push(text);
  }
  return lines.join("\n");
}

export function trackTerminalCommands(terminal: Terminal, terminalId: () => string | undefined, changed: (result?: TerminalCommand) => void) {
  let pending: PendingOutput | undefined;
  let lastSequence = 0;
  let completedSequence = 0;
  function discardOutputStart() {
    const start = pending?.start;
    pending = undefined;
    if (start && "marker" in start) start.marker.dispose();
  }
  const markers = terminal.parser.registerOscHandler(633, (data) => {
    const start = data.startsWith(startPrefix);
    if (!start && !data.startsWith(endPrefix)) return false;
    const value = data.slice((start ? startPrefix : endPrefix).length);
    const [id, sequenceText, rowText, columnText, status, encoded, ...extra] = value.split(";");
    const expectedId = terminalId();
    if (!expectedId || id !== expectedId || !sequenceText || !rowText || !columnText || !/^\d+$/.test(sequenceText)
      || !/^-?\d+$/.test(rowText) || !/^\d+$/.test(columnText) || extra.length) return true;
    const sequence = Number(sequenceText);
    const row = Number(rowText);
    const column = Number(columnText);
    if (!Number.isSafeInteger(sequence) || !Number.isSafeInteger(column)) return true;
    const buffer = terminal.buffer.active;
    // ConPTY can deliver an OSC before its screen patch. At the bottom row a
    // pending scroll can leave both cursor coordinates unchanged.
    const provableRow = buffer.type === "normal" && row < terminal.rows - 1;
    const atReportedPosition = provableRow && buffer.cursorY === row && buffer.cursorX === column;
    const followingLine = provableRow && column === 0 && row > buffer.cursorY;
    if (start) {
      if (status !== undefined || sequence <= Math.max(lastSequence, completedSequence)) return true;
      lastSequence = sequence;
      discardOutputStart();
      changed(undefined);
      const marker = atReportedPosition ? terminal.registerMarker() : undefined;
      if (marker) pending = { sequence, start: { marker, column } };
      else if (followingLine && row === buffer.cursorY + 1) pending = { sequence, start: { row, baseY: buffer.baseY } };
      return true;
    }
    if (encoded === undefined || (status !== "failed" && status !== "succeeded" && status !== "unknown")
      || (sequence !== 0 && (sequence < lastSequence || sequence <= completedSequence))) return true;
    if (sequence > 0) completedSequence = sequence;
    try {
      const command = new TextDecoder("utf-8", { fatal: true }).decode(Uint8Array.from(atob(encoded), (byte) => byte.charCodeAt(0)));
      const result = { command, failed: status === "failed" && sequence > 0 && sequence === lastSequence };
      const matchingStart = command && pending?.sequence === sequence ? pending : undefined;
      // ConPTY can send the end marker before rendering command output. A
      // forward line feed to its exact position can still seal the last line.
      if (matchingStart && followingLine && ("marker" in matchingStart.start || row > matchingStart.start.row)) {
        matchingStart.completion = { result, row, baseY: buffer.baseY };
        changed(result);
        return true;
      }
      const output = atReportedPosition && matchingStart && "marker" in matchingStart.start ? readOutput(terminal, matchingStart.start) : undefined;
      discardOutputStart();
      changed(command ? { ...result, output } : undefined);
    } catch {
      discardOutputStart();
      changed(undefined);
    }
    return true;
  });
  const lineFeeds = terminal.onLineFeed(() => {
    if (!pending) return;
    const buffer = terminal.buffer.active;
    if (!("marker" in pending.start)) {
      const { row, baseY } = pending.start;
      const marker = buffer.baseY === baseY && buffer.cursorY === row && buffer.cursorX === 0 ? terminal.registerMarker() : undefined;
      if (!marker) return discardOutputStart();
      pending.start = { marker, column: 0 };
    }
    if (!pending.completion) return;
    const { result, row, baseY } = pending.completion;
    if (buffer.baseY !== baseY || buffer.cursorY > row) discardOutputStart();
    else if (buffer.cursorY === row) {
      const output = buffer.cursorX === 0 ? readOutput(terminal, pending.start) : undefined;
      discardOutputStart();
      changed({ ...result, output });
    }
  });
  const resizing = terminal.onResize(discardOutputStart);
  const screenChanges = terminal.buffer.onBufferChange(discardOutputStart);
  const redraws = ["A", "F", "H", "J", "L", "M", "S", "T", "d", "f"].map((final) => terminal.parser.registerCsiHandler({ final }, () => {
    discardOutputStart();
    return false;
  }));
  return {
    discardOutputStart,
    dispose() {
      markers.dispose();
      lineFeeds.dispose();
      resizing.dispose();
      screenChanges.dispose();
      for (const redraw of redraws) redraw.dispose();
      discardOutputStart();
    },
  };
}
