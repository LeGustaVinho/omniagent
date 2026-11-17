export type EditorExcerpt = { path: string; text: string; startLine: number; endLine: number; language?: string };

export function excerptReference({ path, startLine, endLine }: Omit<EditorExcerpt, "text">) {
  return `${path}:${startLine}${endLine === startLine ? "" : `-${endLine}`}`;
}

export function markdownExcerpt({ text, language = "" }: Pick<EditorExcerpt, "text" | "language">) {
  const longest = [...text.matchAll(/`+/g)].reduce((size, [span]) => Math.max(size, span.length), 2);
  const fence = "`".repeat(longest + 1);
  return `${fence}${language}\n${text}${text.endsWith("\n") ? "" : "\n"}${fence}`;
}
