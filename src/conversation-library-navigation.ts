import { createContext } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";

export type OpenLibraryConversation = (checkout: ProjectCheckout, conversationId: string) => Promise<void>;

export const ConversationLibraryNavigation = createContext<{
  checkoutId: string;
  projectId: string;
  open: OpenLibraryConversation;
} | null>(null);
