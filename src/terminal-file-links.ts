import type { IBufferCellPosition, IBufferRange, ILinkProvider, Terminal } from "@xterm/xterm";

export type TerminalFileReference = { path: string; line?: number; column?: number; text: string };
type TerminalFileHit = { range: IBufferRange; reference: TerminalFileReference };

const maximumWrappedRows = 32;
const maximumLineLength = 8192;
const positionSuffix = /(?::(\d+)(?::(\d+))?|\((\d+),\s*(\d+)\))$/u;
const quote = /^(?:"([^"]+)"|'([^']+)')((?::\d+){1,2}|\(\d+,\s*\d+\))?$/u;
const standaloneAbsolutePath = /^\s*([a-z]:[\\/][^:\r\n"'<>|?*(){}\[\],;]*? [^:\r\n"'<>|?*(){}\[\],;]*?\.[a-z][\w-]{0,15})\s*$/iu;
const locatedPathWithSpaces = /(?<![\p{L}\p{N}_:/\\])(?:[a-z]:[\\/]|[\p{L}\p{N}_.-]+[\\/])[^:\r\n"'<>|?*(){}\[\],;]*? [^:\r\n"'<>|?*(){}\[\],;]*?(?::\d+(?::\d+)?|\(\d+,\s*\d+\))(?=$|[\s)}\],;.!])/iu;
const token = /(?:"[^"\r\n]+"|'[^'\r\n]+'|[^\s"'`()[\]{},;<>]+)(?:(?::\d+){1,2}|\(\d+,\s*\d+\))?/u;
const terminalTokens = new RegExp(`${locatedPathWithSpaces.source}|${token.source}`, "giu");

export function parseTerminalFileReference(text: string): TerminalFileReference | undefined {
  const quoted = text.match(quote);
  const location = quoted ? (quoted[1] ?? quoted[2]) + (quoted[3] ?? "") : text;
  const suffix = location.match(positionSuffix);
  const path = suffix ? location.slice(0, suffix.index) : location;
  const line = suffix ? Number(suffix[1] ?? suffix[3]) : undefined;
  const column = suffix?.[2] !== undefined || suffix?.[4] !== undefined ? Number(suffix[2] ?? suffix[4]) : undefined;
  if ((line !== undefined && (line < 1 || line > 0xffffffff)) || (column !== undefined && (column < 1 || column > 0xffffffff))) return;
  const relative = path.replace(/^[A-Za-z]:[\\/]/u, "");
  if (!relative || relative !== relative.trim() || /[:?*"<>|\u0000-\u001F\u007F]/u.test(relative)
    || /^[\\/]/u.test(relative) || /[\\/.]$/u.test(relative)) return;
  const fileLike = /[\\/]/u.test(path) || /\.[a-z][\w-]{0,15}$/iu.test(path)
    || /^(?:\.env(?:\.[\w-]+)?|README|LICENSE|COPYING|CHANGELOG|Dockerfile|Makefile|Gemfile|Rakefile|Procfile)$/iu.test(path);
  if (!fileLike) return;
  return { path, line, column, text };
}

function fileHits(terminal: Terminal, lineNumber: number): TerminalFileHit[] {
  const { active: buffer } = terminal.buffer;
  let row = lineNumber - 1;
  let rewound = 0;
  while (row > 0 && buffer.getLine(row)?.isWrapped) {
    if (++rewound >= maximumWrappedRows) return [];
    row--;
  }
  if (buffer.getLine(row)?.isWrapped) return [];
  let text = "";
  const positions: { start: IBufferCellPosition; end: IBufferCellPosition }[] = [];
  for (let count = 0; ; count++, row++) {
    if (count >= maximumWrappedRows) return [];
    const line = buffer.getLine(row);
    if (!line) break;
    const following = buffer.getLine(row + 1);
    for (let x = 0; x < Math.min(terminal.cols, line.length); x++) {
      const cell = line.getCell(x);
      const width = cell?.getWidth() ?? 0;
      if (width === 0) continue;
      const chars = cell?.getChars();
      if (!chars && x === terminal.cols - 1 && following?.isWrapped && following.getCell(0)?.getWidth() === 2) continue;
      const value = chars || " ";
      if (text.length + value.length > maximumLineLength) return [];
      const position = { start: { x: x + 1, y: row + 1 }, end: { x: x + width, y: row + 1 } };
      for (let unit = 0; unit < value.length; unit++) positions.push(position);
      text += value;
    }
    if (!following?.isWrapped) break;
  }
  const hits: TerminalFileHit[] = [];
  const standalone = text.match(standaloneAbsolutePath);
  const tokens = standalone ? [{ index: text.indexOf(standalone[1]!), value: standalone[1]! }]
    : Array.from(text.matchAll(terminalTokens), (match) => ({ index: match.index, value: match[0] }));
  for (const token of tokens) {
    const value = token.value.replace(/\.+$/u, "");
    const reference = parseTerminalFileReference(value);
    if (!reference) continue;
    const start = positions[token.index]?.start;
    const end = positions[token.index + value.length - 1]?.end;
    if (start && end) hits.push({ range: { start, end }, reference });
  }
  return hits;
}

export function terminalFileLinkProvider(terminal: Terminal, open: (reference: TerminalFileReference) => void): ILinkProvider {
  return { provideLinks(line, callback) {
    const links = fileHits(terminal, line).map(({ range, reference }) => ({ range, text: reference.text,
      activate: (event: MouseEvent) => { if (event.button === 0 && (event.ctrlKey || event.metaKey)) open(reference); },
    }));
    callback(links.length ? links : undefined);
  } };
}

export function terminalFileAtPoint(terminal: Terminal, point: { clientX: number; clientY: number }): TerminalFileReference | undefined {
  const screen = terminal.element?.querySelector(".xterm-screen")?.getBoundingClientRect();
  if (!screen?.width || !screen.height) return;
  const x = Math.floor(((point.clientX - screen.left) * terminal.cols) / screen.width) + 1;
  const row = Math.floor(((point.clientY - screen.top) * terminal.rows) / screen.height) + 1;
  if (x < 1 || x > terminal.cols || row < 1 || row > terminal.rows) return;
  const y = terminal.buffer.active.viewportY + row;
  return fileHits(terminal, y).find(({ range: { start, end } }) =>
    (y > start.y || (y === start.y && x >= start.x)) && (y < end.y || (y === end.y && x <= end.x)))?.reference;
}
