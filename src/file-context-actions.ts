import { invoke } from "@tauri-apps/api/core";
import { useEffect } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { MenuGroups } from "@/components/ui/context-actions";
import { omniAgentFileLink } from "@/file-links";
import { gitFilePermalink, type GitPermalinkLines } from "@/git-file-versions";
import { fileName } from "@/ide-knowledge-data";
import type { FileTreeTarget } from "@/ide-file-workspace";
import { noteSearchTarget } from "@/ide-file-search";
import type { WorkspaceProps } from "@/workbench-contract";

const terminalPrefix = "omni-terminal-folder:";
const terminalRequestPrefix = "omni-terminal-request:";
const newWorktreePrefix = "omni-new-worktree:";
const gitCheckoutPrefix = "omni-git-checkout:";
let workspaceShortcut = 0;
const noteRenamePrefix = "omni-rename-note:";
const openNotesPrefix = "omni-open-notes:";
let noteNavigationRequest = 0;
const workingDiffPrefix = "omni-working-diff:";
const fileHistoryPrefix = "omni-file-history:";
const sourceLinePrefix = "omni-source-line:";
let sourceNavigationRequest = 0;

export function workingDiffTarget(path: string) {
  return workingDiffPrefix + path;
}

export function readWorkingDiffTarget(target?: string) {
  if (target?.startsWith(workingDiffPrefix)) return target.slice(workingDiffPrefix.length);
}

export function fileHistoryTarget(path: string) {
  return fileHistoryPrefix + path;
}

export function readFileHistoryTarget(target?: string) {
  if (target?.startsWith(fileHistoryPrefix)) return target.slice(fileHistoryPrefix.length) || undefined;
}

export function sourceLineTarget(path: string, line?: number, column?: number) {
  return sourceLinePrefix + JSON.stringify([path, line, ++sourceNavigationRequest, column]);
}

export function readSourceLineTarget(target?: string) {
  if (!target?.startsWith(sourceLinePrefix)) return;
  try {
    const [path, line, , column] = JSON.parse(target.slice(sourceLinePrefix.length)) as [unknown, unknown, unknown, unknown];
    if (typeof path === "string" && (line === null || typeof line === "number" && Number.isSafeInteger(line) && line >= 1)
      && (column == null || typeof column === "number" && Number.isSafeInteger(column) && column >= 1)) {
      return { path, line: line ?? undefined, column: column ?? undefined };
    }
  } catch { return; }
}

export function openWorkspaceFile(openWorkspace: WorkspaceProps["openWorkspace"], path: string, line?: number,
  { column, repeat }: { column?: number; repeat?: boolean } = {}) {
  if (/\.(md|markdown)$/i.test(path)) openWorkspace("knowledge", line || repeat ? noteSearchTarget(path, line, ++sourceNavigationRequest) : path);
  else openWorkspace("ide", line || repeat ? sourceLineTarget(path, line, column) : path);
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
  return terminalRequestPrefix + JSON.stringify([path, ++workspaceShortcut]);
}

export function readTerminalFolderTarget(target?: string) {
  if (target?.startsWith(terminalPrefix)) return target.slice(terminalPrefix.length);
  if (!target?.startsWith(terminalRequestPrefix)) return;
  try {
    const [path] = JSON.parse(target.slice(terminalRequestPrefix.length)) as [unknown];
    if (typeof path === "string") return path;
  } catch { return; }
}

export const newWorktreeTarget = () => newWorktreePrefix + ++workspaceShortcut;
export const isNewWorktreeTarget = (target?: string) => Boolean(target?.startsWith(newWorktreePrefix));
export const gitCheckoutTarget = () => gitCheckoutPrefix + ++workspaceShortcut;
export const isGitCheckoutTarget = (target?: string) => Boolean(target?.startsWith(gitCheckoutPrefix));

export function containingFolder(path: string) {
  const slash = path.lastIndexOf("/");
  return slash < 0 ? "" : path.slice(0, slash);
}

type CurrentText = (path: string) => string | undefined;
const checkoutTextReaders = new Map<string, Set<CurrentText>>();
let textCaptureRequest = 0;

export function useCheckoutTextReader(checkoutId: string, enabled: boolean, read: CurrentText) {
  useEffect(() => {
    if (!enabled) return;
    const readers = checkoutTextReaders.get(checkoutId) ?? new Set<CurrentText>();
    readers.add(read);
    checkoutTextReaders.set(checkoutId, readers);
    return () => {
      readers.delete(read);
      if (!readers.size) checkoutTextReaders.delete(checkoutId);
    };
  }, [checkoutId, enabled, read]);
}

function otherEditorText(checkoutId: string, path: string) {
  for (const read of checkoutTextReaders.get(checkoutId) ?? []) {
    const text = read(path);
    if (text !== undefined) return text;
  }
}

type CheckoutFileText = { status: "text"; text: string } | { status: "unsupported_encoding" };

export function checkoutFileLiveText(checkoutId: string, path: string, currentText?: CurrentText) {
  return currentText?.(path) ?? otherEditorText(checkoutId, path);
}

export function readCheckoutFileText(checkoutId: string, path: string) {
  return invoke<CheckoutFileText>("read_checkout_file_text", { checkoutId, path });
}

export function fileContextActions({ checkout, status, report, currentText }: {
  checkout: ProjectCheckout;
  status: (message: string) => void;
  report: (message: string) => void;
  currentText?: CurrentText;
}) {
  const copy = (value: string, message: string) => {
    ++textCaptureRequest;
    report("");
    void Promise.resolve().then(() => navigator.clipboard.writeText(value)).then(() => status(message), (error: unknown) => {
      report(`Could not copy to the clipboard. ${String(error)}`);
    });
  };
  const shell = (relativePath: string, action: "reveal" | "open" | "open_with") => {
    report("");
    void invoke("open_checkout_item_in_shell", { checkoutId: checkout.checkoutId, relativePath, action }).catch((error: unknown) => report(String(error)));
  };
  const copyPermalink = (path: string, lines?: GitPermalinkLines) => {
    report("");
    void gitFilePermalink(checkout.checkoutId, path, undefined, lines).then(
      (url) => copy(url, "Copied committed permalink"), (error: unknown) => report(String(error)),
    );
  };
  const copyOmniAgentLink = (path: string, line?: number, column?: number) => {
    copy(omniAgentFileLink(checkout.checkoutId, path, line, column), "Copied OmniAgent link");
  };
  function copyTextContents(path: string) {
    const request = ++textCaptureRequest;
    const text = checkoutFileLiveText(checkout.checkoutId, path, currentText);
    const confirmation = "Copied text contents of " + fileName(path);
    if (text !== undefined) {
      copy(text, confirmation);
      return;
    }
    report("");
    void readCheckoutFileText(checkout.checkoutId, path).then((file) => {
      if (request !== textCaptureRequest) return;
      if (file.status === "text") copy(file.text, confirmation);
      else report(path + " contains binary data or uses an unsupported text encoding.");
    }).catch((failure: unknown) => { if (request === textCaptureRequest) report(String(failure)); });
  }
  function location(target: FileTreeTarget): MenuGroups {
    const { path } = target;
    const absolute = (path ? `${checkout.checkoutRoot.replace(/[\\/]+$/, "")}/${path}` : checkout.checkoutRoot).replaceAll("/", "\\");
    return [[
      ...(path ? [{ label: "Copy relative path", run: () => copy(path, "Copied relative path") }] : []),
      ...(target.kind === "file" && currentText ? [{ label: "Copy text contents", run: () => copyTextContents(path) }] : []),
      { label: "Show in File Explorer", run: () => shell(path, "reveal") },
      ...(target.kind === "file" ? [
        { label: "Open in default application", run: () => shell(path, "open") },
        { label: "Open with another application…", run: () => shell(path, "open_with") },
        { label: "Open in external editor", run: () => {
          report("");
          void invoke("open_checkout_file_in_external_editor", { checkoutId: checkout.checkoutId, relativePath: path })
            .then(() => status("Started external editor for " + fileName(path)))
            .catch((error: unknown) => report(String(error)));
        } },
      ] : []),
    ], [{ label: "Copy", items: [
      ...(path ? [
        { label: "Name", run: () => copy(fileName(path), "Copied name") },
      ] : []),
      { label: "Absolute path", run: () => copy(absolute, "Copied absolute path") },
      ...(target.kind === "file" ? [
        ...(checkout.checkoutId ? [{ label: "OmniAgent link", run: () => copyOmniAgentLink(path) }] : []),
        { label: "Committed file permalink", run: () => copyPermalink(path) },
      ] : []),
    ] }]];
  }
  return { copy, copyOmniAgentLink, copyPermalink, location, reveal: (path: string) => shell(path, "reveal") };
}
