import { invoke } from "@tauri-apps/api/core";
import { useContext } from "react";
import { useContextActionFeedback, type MenuAction } from "@/components/ui/context-actions";
import type { AttachmentSource } from "@/conversation-attachments";
import { fileHistoryTarget } from "@/file-context-actions";
import { RunInspectorNavigation } from "@/run-inspector-navigation";
import { useWorkspace } from "@/workspace-state";

export function useAttachmentActions(source: AttachmentSource, name: string, checkoutId?: string) {
  const { copy, feedback, run } = useContextActionFeedback();
  const navigation = useContext(RunInspectorNavigation);
  const activeCheckoutId = useWorkspace((state) => state.checkout?.checkoutId);
  const openWorkspace = checkoutId && activeCheckoutId === checkoutId ? navigation?.openWorkspace : undefined;
  const path = source.kind === "checkout" ? source.relativePath : source.path;
  const checkout: MenuAction[] = !checkoutId || source.kind !== "checkout" ? [] : [
    ...(openWorkspace ? [
      { label: "Open current checkout file", run: () => openWorkspace(/\.(md|markdown)$/i.test(path) ? "knowledge" : "ide", path) },
      { label: "Show file history", run: () => openWorkspace("git", fileHistoryTarget(path)) },
    ] : []),
    { label: "Show source in File Explorer", run: () => void run(() => invoke("open_checkout_item_in_shell", {
      checkoutId, relativePath: path, action: "reveal",
    })) },
  ];
  const clipboard: MenuAction[] = [
    { label: "Copy file name", run: () => copy(name) },
    { label: source.kind === "checkout" ? "Copy checkout-relative path" : "Copy original path", run: () => copy(path) },
  ];
  return { path, checkout, clipboard, copy, feedback, run };
}
