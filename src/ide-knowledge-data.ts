export interface NoteSource {
  path: string;
  content: string;
}

export function fileName(path: string) {
  return path.split("/").at(-1) ?? path;
}

export function normalizeRepositoryPath(path: string): string | undefined {
  if (/^(?:[\\/]|[a-z]:)/i.test(path) || path.includes("\0")) return;
  const output: string[] = [];
  for (const part of path.replaceAll("\\", "/").split("/")) {
    if (part === "..") {
      if (!output.length) return;
      output.pop();
    } else if (part && part !== ".") {
      output.push(part);
    }
  }
  return output.length ? output.join("/") : undefined;
}

export interface FileSearchResult {
  path: string;
  line: number;
  excerpt: string;
  columns?: readonly number[];
  occurrences?: number;
  lineText?: string | null;
}
