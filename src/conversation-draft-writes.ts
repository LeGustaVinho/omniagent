import { invoke } from "@tauri-apps/api/core";

const draftWrites = new Map<string, Promise<void>>();

export function queueConversationDraftWrite<T>(conversationId: string, perform: () => Promise<T>) {
  const pending = draftWrites.get(conversationId);
  const write = (pending?.catch(() => undefined) ?? Promise.resolve()).then(perform);
  const current = write.then(() => undefined);
  draftWrites.set(conversationId, current);
  const release = () => {
    if (draftWrites.get(conversationId) === current) draftWrites.delete(conversationId);
  };
  void current.then(release, release);
  return write;
}

export function saveDraft(conversationId: string, text: string) {
  return queueConversationDraftWrite(conversationId, () => invoke<void>("save_conversation_draft", { conversationId, text }));
}

export async function readDraft(conversationId: string) {
  await draftWrites.get(conversationId);
  return invoke<string>("conversation_draft", { conversationId });
}
