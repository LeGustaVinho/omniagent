import { invoke } from "@tauri-apps/api/core";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { MenuGroups } from "@/components/ui/context-actions";
import { fileName } from "@/ide-knowledge-data";
import type { FileTreeTarget } from "@/ide-file-workspace";

const terminalPrefix = "omni-terminal-folder:";
const noteRenamePrefix = "omni-rename-note:";
const openNotesPrefix = "omni-open-notes:";
let noteNavigationRequest = 0;
const workingDiffPrefix = "omni-working-diff:";

export function workingDiffTarget(path: string) {
  return workingDiffPrefix + path;
}

export function readWorkingDiffTarget(target?: string) {
  if (target?.startsWith(workingDiffPrefix)) return target.slice(workingDiffPrefix.length);
}

export function noteRenameTarget(path: string) {
  return noteRenamePrefix + encodeURIComponent(path) + "#" + ++noteNavigationRequest;
}

export function readNoteRenameTarget(target?: string) {
  if (!target?.startsWith(noteRenamePrefix)) return;
  try { return decodeURIComponent(target.slice(noteRenamePrefix.length).split("#", 1)[0]!); }
  catch { return undefined; }
}

export function openNotesTarget(paths: readonly string[]) {
  return openNotesPrefix + JSON.stringify([++noteNavigationRequest, paths]);
}

export function readOpenNotesTarget(target?: string): string[] | undefined {
  if (!target?.startsWith(openNotesPrefix)) return;
  try {
    const payload: unknown = JSON.parse(target.slice(openNotesPrefix.length));
    if (Array.isArray(payload) && Array.isArray(payload[1])) return payload[1].filter((path): path is string => typeof path === "string" && /\.(?:md|markdown)$/i.test(path));
  } catch { return undefined; }
}

export function terminalFolderTarget(path: string) {
  return terminalPrefix + path;
}

export function readTerminalFolderTarget(target?: string) {
  if (target?.startsWith(terminalPrefix)) return target.slice(terminalPrefix.length);
}

export function containingFolder(path: string) {
  const slash = path.lastIndexOf("/");
  return slash < 0 ? "" : path.slice(0, slash);
}

export function fileContextActions({ checkout, simulated, status, report }: {
  checkout: ProjectCheckout;
  simulated: boolean;
  status: (message: string) => void;
  report: (message: string) => void;
}) {
  const copy = (value: string, message: string) => {
    report("");
    void Promise.resolve().then(() => navigator.clipboard.writeText(value)).then(() => status(message), (error: unknown) => {
      report(`Could not copy to the clipboard. ${String(error)}`);
    });
  };
  const shell = (relativePath: string, action: "reveal" | "open") => {
    report("");
    void invoke("open_checkout_item_in_shell", { checkoutId: checkout.checkoutId, relativePath, action }).catch((error: unknown) => report(String(error)));
  };
  function location(target: FileTreeTarget): MenuGroups {
    const { path } = target;
    const absolute = (path ? `${checkout.checkoutRoot.replace(/[\\/]+$/, "")}/${path}` : checkout.checkoutRoot).replaceAll("/", "\\");
    return [[
      ...(path ? [{ label: "Copy relative path", run: () => copy(path, "Copied relative path") }] : []),
      ...(!simulated ? [
        { label: "Show in File Explorer", run: () => shell(path, "reveal") },
        ...(target.kind === "file" ? [{ label: "Open in default application", run: () => shell(path, "open") }] : []),
      ] : []),
    ], [{ label: "Copy", items: [
      ...(path ? [
        { label: "Name", run: () => copy(fileName(path), "Copied name") },
      ] : []),
      ...(!simulated ? [{ label: "Absolute path", run: () => copy(absolute, "Copied absolute path") }] : []),
    ], disabled: !path && simulated }]];
  }
  return { copy, location };
}
