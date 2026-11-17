import type { ProjectCheckout } from "@/bindings/ProjectCheckout";

export type FileLinkTarget = {
  checkout: ProjectCheckout;
  path: string;
  line: number | null;
  column: number | null;
};

export const fileLinkEvent = "omniagent-file-link";

export function omniAgentFileLink(checkoutId: string, path: string, line?: number, column?: number) {
  const query = new URLSearchParams({ checkout: checkoutId, path });
  if (line !== undefined) query.set("line", String(line));
  if (line !== undefined && column !== undefined && !/\.(md|markdown)$/i.test(path)) query.set("column", String(column));
  return `omniagent://open-file/?${query}`;
}
