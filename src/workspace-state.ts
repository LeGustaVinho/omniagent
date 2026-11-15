import { create } from "zustand";
import { persist } from "zustand/middleware";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";

interface WorkspaceState {
  checkout: ProjectCheckout | null;
  selectCheckout: (checkout: ProjectCheckout | null) => void;
}

export const useWorkspace = create<WorkspaceState>()(
  persist(
    (set) => ({
      checkout: null,
      selectCheckout: (checkout) => set({ checkout }),
    }),
    { name: "omniagent-workspace-v1" },
  ),
);
