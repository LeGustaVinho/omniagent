import { useMutation } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { Conversation } from "@/bindings/Conversation";

export function useConversationTranscriptExport(notice: (message: string) => void) {
  return useMutation({
    mutationFn: ({ id, checkoutId }: Pick<Conversation, "id" | "checkoutId">) => {
      notice("");
      return invoke<boolean>("export_conversation_markdown", { conversationId: id, checkoutId });
    },
    onSuccess: (saved) => { if (saved) notice("Conversation exported as Markdown."); },
  });
}
