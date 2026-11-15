import type { ModelOption } from "@/bindings/ModelOption";
import type { Provider } from "@/bindings/Provider";
import catalog from "../src-tauri/model-catalog/v1.json";

export type PreviewMessage = { id: string; author: "user" | "assistant"; text: string };
export type AttachmentRoute = "Native" | "Excerpt" | "Reference";
export type PreviewAttachment = { path: string; route: AttachmentRoute };
export type PreviewGoal = {
  objective: string;
  previousObjectives: string[];
  model: string;
  state: "Running" | "Paused" | "Budget reached" | "Cancelled";
  spent: number;
  budget: number;
};
export type PreviewConversation = {
  id: string;
  title: string;
  project: string;
  worktree: string;
  section?: string;
  pinned: boolean;
  unread: boolean;
  archived: boolean;
  draft: string;
  attachments: PreviewAttachment[];
  messages: PreviewMessage[];
  former: PreviewMessage[][];
  queued: string[];
  revision: number;
  forkOf?: string;
  goal?: PreviewGoal;
  sideQuestion?: string;
};

export type PreviewLibrary = {
  conversations: PreviewConversation[];
  sections: Record<string, string[]>;
  activeId: string | null;
  tabIds: string[];
  nextId: number;
};

export const primaryProject = "Acme Docs";
export const deliveryConversation = "plan";
export const previewPlan = "### Update the release checklist\n\n"
  + "1. Add a **Recovery** section to `docs/release.md`.\n"
  + "2. Verify the links and Markdown rendering.\n"
  + "3. Review the saved patch before accepting the change.";

export const fileSamples = {
  "docs/release.md": "# Release checklist\n\n- Verify the package.\n- Confirm the required checks.\n",
  "docs/troubleshooting.md": "# Troubleshooting\n\nConfirm the previous version is available before restoring it.",
  "src/release.test.ts": "test('retains the previous build', () => {\n  expect(candidate.canRestore).toBe(true);\n});",
};

export const providers: Record<Provider, string> = { codex: "Codex", claude: "Claude Code", antigravity: "Gemini · Antigravity" };
export const demoModels: ModelOption[] = catalog.models.map((model) => ({
  selection: { provider: model.provider as Provider, profileId: "simulation", modelId: model.id },
  name: model.name,
  selectable: true,
  unavailableReason: null,
  catalogVersion: catalog.version,
  rating: { intelligenceTenths: model.intelligenceTenths, priceScore: model.priceScore, apiReferenceCents: model.apiReferenceCents },
}));

export function createPreviewConversation(id: string, title: string, project = primaryProject): PreviewConversation {
  return {
    id, title, project, worktree: "example-main", pinned: false, unread: false, archived: false,
    draft: "", attachments: [], messages: [], former: [], queued: [], revision: 1,
  };
}

export function createLibrary(): PreviewLibrary {
  return {
    conversations: [
      {
        ...createPreviewConversation(deliveryConversation, "Plan the release checklist"),
        pinned: true,
        messages: [
          { id: "initial-1", author: "user", text: "Read the release checklist and plan a recovery section." },
          { id: "initial-2", author: "assistant", text: "I found `docs/release.md`. I’ll confirm the target audience before proposing changes." },
        ],
      },
      {
        ...createPreviewConversation("check-provider", "Check provider profiles"),
        unread: true, section: "Research",
        messages: [{ id: "profile-1", author: "assistant", text: "The example profiles are available. Select a provider to check its reported options." }],
      },
      { ...createPreviewConversation("old-review", "Review the previous installer"), archived: true },
      { ...createPreviewConversation("inventory", "Inspect example build scripts", "Build Toolkit") },
    ],
    sections: { [primaryProject]: ["Research"], "Build Toolkit": [] },
    activeId: deliveryConversation,
    tabIds: [deliveryConversation],
    nextId: 1,
  };
}
