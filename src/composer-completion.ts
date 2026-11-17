export type ComposerCompletion = { trigger: "@" | "/"; query: string; start: number; end: number };

function insideCode(text: string) {
  let fence: string | undefined;
  let ticks = 0;
  let indented = false;
  for (const line of text.split("\n")) {
    const delimiter = /^ {0,3}(`{3,}|~{3,})(.*)$/.exec(line);
    if (fence) {
      if (delimiter && delimiter[1]![0] === fence[0] && delimiter[1]!.length >= fence.length && !delimiter[2]!.trim()) {
        fence = undefined;
      }
      continue;
    }
    if (!ticks && delimiter) {
      fence = delimiter[1];
      continue;
    }
    indented = !ticks && /^(?: {4}|\t)/.test(line);
    if (indented) continue;
    for (const match of line.matchAll(/`+/g)) {
      if (!ticks && /(?:^|[^\\])(?:\\\\)*\\$/.test(line.slice(0, match.index))) continue;
      if (!ticks) ticks = match[0].length;
      else if (match[0].length === ticks) ticks = 0;
    }
  }
  return Boolean(ticks || fence || indented);
}

export function composerCompletion(text: string, cursor: number): ComposerCompletion | undefined {
  if (cursor < 0 || cursor > text.length) return;
  const before = text.slice(0, cursor);
  const match = /(?:^|[\s([{])([@/])([\p{L}\p{N}_:.-]*)$/u.exec(before);
  if (!match || insideCode(before)) return;
  const trigger = match[1] as ComposerCompletion["trigger"];
  const query = match[2]!;
  const rest = text.slice(cursor);
  const right = /^[\p{L}\p{N}_:.-]*/u.exec(rest)?.[0] ?? "";
  const token = query + right;
  if (token.startsWith("skill:") || token.startsWith("command:") || token.startsWith("stage:")) return;
  if (/^[\\/@]/.test(rest.slice(right.length)) || token.includes(":")) return;
  return { trigger, query, start: cursor - query.length - 1, end: cursor + right.length };
}

export function replaceComposerCompletion(text: string, range: { start: number; end: number }, replacement: string) {
  const prefix = text.slice(0, range.start);
  const suffix = text.slice(range.end);
  if (!replacement) {
    const rest = !prefix || /\s$/.test(prefix) ? suffix.replace(/^ /, "") : suffix;
    return { text: prefix + rest, cursor: prefix.length };
  }
  const marker = /^@(?:skill|command|stage):/.test(replacement);
  const before = (marker ? /\S$/ : /[\p{L}\p{N}_]$/u).test(prefix) ? `${prefix} ` : prefix;
  const after = suffix.replace(/^ /, "");
  const insert = !marker && /^[\n\r\t,.;!?)\]}]/.test(after) ? replacement : `${replacement} `;
  return { text: before + insert + after, cursor: before.length + insert.length };
}
