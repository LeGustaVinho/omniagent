import type { IBufferCellPosition, IBufferRange, ILinkProvider, IMarker, Terminal } from "@xterm/xterm";

type TerminalPathKind = "file" | "folder";
export type ResolvedTerminalPath = { path: string; kind: TerminalPathKind };
export type TerminalPathReference = { path: string; line?: number; column?: number; text: string; bufferLine?: number; expectedKind?: TerminalPathKind };
type TerminalPathHit = { range: IBufferRange; reference: TerminalPathReference };

const maximumWrappedRows = 32;
const maximumLineLength = 8192;
const positionSuffix = /(?::(\d+)(?::(\d+))?|\((\d+),\s*(\d+)\))$/u;
const quote = /^(?:"([^"]+)"|'([^']+)')((?::\d+){1,2}|\(\d+,\s*\d+\))?$/u;
const standaloneAbsolutePath = /^\s*([a-z]:[\\/][^:\r\n"'<>|?*(){}\[\],;]*? [^:\r\n"'<>|?*(){}\[\],;]*?)\s*$/iu;
const locatedPathWithSpaces = /(?<![\p{L}\p{N}_:/\\])(?:[a-z]:[\\/]|[\p{L}\p{N}_.-]+[\\/])[^:\r\n"'<>|?*(){}\[\],;]*? [^:\r\n"'<>|?*(){}\[\],;]*?(?::\d+(?::\d+)?|\(\d+,\s*\d+\))(?=$|[\s)}\],;.!])/iu;
const token = /(?:"[^"\r\n]+"|'[^'\r\n]+'|[^\s"'`()[\]{},;<>]+)(?:(?::\d+){1,2}|\(\d+,\s*\d+\))?/u;
const terminalTokens = new RegExp(`${locatedPathWithSpaces.source}|${token.source}`, "giu");
const directoryComponent = /(?:^|[\\/])\.{1,2}$/u;

export function parseTerminalPathReference(text: string, standalone = false): TerminalPathReference | undefined {
  const quoted = text.match(quote);
  const location = quoted ? (quoted[1] ?? quoted[2]) + (quoted[3] ?? "") : text;
  const suffix = location.match(positionSuffix);
  const path = suffix ? location.slice(0, suffix.index) : location;
  const line = suffix ? Number(suffix[1] ?? suffix[3]) : undefined;
  const column = suffix?.[2] !== undefined || suffix?.[4] !== undefined ? Number(suffix[2] ?? suffix[4]) : undefined;
  if ((line !== undefined && (line < 1 || line > 0xffffffff)) || (column !== undefined && (column < 1 || column > 0xffffffff))) return;
  const relative = path.replace(/^[A-Za-z]:[\\/]/u, "");
  const endsWithDirectory = /[\\/]$/u.test(path) || directoryComponent.test(relative);
  if (!path || path !== path.trim() || /[:?*"<>|\u0000-\u001F\u007F]/u.test(relative)
    || /^[\\/]/u.test(relative) || (!endsWithDirectory && /\.$/u.test(relative)) || (endsWithDirectory && line !== undefined)) return;
  const pathLike = /[\\/]/u.test(path) || (standalone && Boolean(quoted || endsWithDirectory)) || /\.[a-z][\w-]{0,15}$/iu.test(path)
    || /^(?:\.env(?:\.[\w-]+)?|README|LICENSE|COPYING|CHANGELOG|Dockerfile|Makefile|Gemfile|Rakefile|Procfile)$/iu.test(path);
  if (!pathLike) return;
  return { path, line, column, text, expectedKind: line !== undefined ? "file" : endsWithDirectory ? "folder" : undefined };
}

function pathHits(terminal: Terminal, lineNumber: number): TerminalPathHit[] {
  const { active: buffer } = terminal.buffer;
  if (buffer.type !== "normal") return [];
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
  const hits: TerminalPathHit[] = [];
  const standalone = text.match(standaloneAbsolutePath);
  const tokens = standalone ? [{ index: text.indexOf(standalone[1]!), value: standalone[1]! }]
    : Array.from(text.matchAll(terminalTokens), (match) => ({ index: match.index, value: match[0] }));
  for (const token of tokens) {
    const value = directoryComponent.test(token.value) ? token.value : token.value.replace(/\.+$/u, "");
    const reference = parseTerminalPathReference(value, text.trim() === value);
    if (!reference) continue;
    const start = positions[token.index]?.start;
    const end = positions[token.index + value.length - 1]?.end;
    if (start && end) hits.push({ range: { start, end }, reference: { ...reference, bufferLine: start.y - 1 } });
  }
  return hits;
}

export function terminalPathLinkProvider(terminal: Terminal, open: (reference: TerminalPathReference) => void): ILinkProvider {
  return { provideLinks(line, callback) {
    const links = pathHits(terminal, line).map(({ range, reference }) => ({ range, text: reference.text,
      activate: (event: MouseEvent) => { if (event.button === 0 && (event.ctrlKey || event.metaKey)) open(reference); },
    }));
    callback(links.length ? links : undefined);
  } };
}

export function terminalPathAtPoint(terminal: Terminal, point: { clientX: number; clientY: number }): TerminalPathReference | undefined {
  const screen = terminal.element?.querySelector(".xterm-screen")?.getBoundingClientRect();
  if (!screen?.width || !screen.height) return;
  const x = Math.floor(((point.clientX - screen.left) * terminal.cols) / screen.width) + 1;
  const row = Math.floor(((point.clientY - screen.top) * terminal.rows) / screen.height) + 1;
  if (x < 1 || x > terminal.cols || row < 1 || row > terminal.rows) return;
  const y = terminal.buffer.active.viewportY + row;
  return pathHits(terminal, y).find(({ range: { start, end } }) =>
    (y > start.y || (y === start.y && x >= start.x)) && (y < end.y || (y === end.y && x <= end.x)))?.reference;
}

const promptDirectoryPrefix = "P;OmniAgentCwd=";
const maximumDirectoryObservations = 2048;

export function trackTerminalDirectories(terminal: Terminal, terminalId: () => string | undefined) {
  const observations: { marker: IMarker; directory: string }[] = [];
  let baseline: string | undefined;
  const handler = terminal.parser.registerOscHandler(633, (data) => {
    if (!data.startsWith(promptDirectoryPrefix)) return false;
    const id = terminalId();
    if (!id || !data.startsWith(`${promptDirectoryPrefix}${id};`) || data.length > 65_536 || terminal.buffer.active.type !== "normal") return true;
    let directory: string;
    try {
      const encoded = data.slice(promptDirectoryPrefix.length + id.length + 1);
      directory = new TextDecoder("utf-8", { fatal: true }).decode(Uint8Array.from(atob(encoded), (byte) => byte.charCodeAt(0)));
    } catch { return true; }
    const latest = observations.at(-1);
    if ((latest?.directory ?? baseline) === directory) return true;
    if (latest?.marker.line === terminal.buffer.active.baseY + terminal.buffer.active.cursorY) {
      latest.directory = directory;
      return true;
    }
    const marker = terminal.registerMarker();
    if (!marker) return true;
    const observation = { marker, directory };
    observations.push(observation);
    marker.onDispose(() => {
      const index = observations.indexOf(observation);
      if (index === 0) baseline = observation.directory;
      if (index >= 0) observations.splice(index, 1);
    });
    if (observations.length > maximumDirectoryObservations) observations[0]!.marker.dispose();
    return true;
  });
  return {
    at(line = terminal.buffer.active.baseY + terminal.buffer.active.cursorY) {
      if (terminal.buffer.active.type !== "normal") return undefined;
      return observations.findLast(({ marker }) => marker.line <= line)?.directory ?? baseline;
    },
    dispose() {
      handler.dispose();
      for (const { marker } of [...observations]) marker.dispose();
    },
  };
}
