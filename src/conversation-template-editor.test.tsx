// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { ConversationTemplateEditor } from "@/conversation-template-editor";
import PipelinesWorkspace from "@/pipelines-workspace";
import type { ModelOption } from "@/bindings/ModelOption";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const checkout = { projectId: "project", checkoutId: "registered", checkoutRoot: "D:\\repo", repositoryRoot: "D:\\repo" };
const option: ModelOption = {
  name: "Codex qualified model", selectable: true, unavailableReason: null, catalogVersion: 1,
  selection: { provider: "codex", profileId: "pinned-profile", modelId: "model" },
  rating: { intelligenceTenths: 915, priceScore: 72, apiReferenceCents: 125 },
};
const first = {
  name: "Review answer",
  role: "pipeline",
  model: option.selection,
  effort: "high",
  instructions: "Use the saved sources.",
  toolScope: "read_only",
  completionContract: "Return a concise report.",
  reviewPolicy: "cross_review",
};
const scope = { projectId: checkout.projectId, checkoutId: checkout.checkoutId };
const firstVersion = { revision: "exact-revision-1", ordinal: 1, baseRevision: null };
const secondVersion = { revision: "exact-revision-2", ordinal: 2, baseRevision: firstVersion.revision };

function setup({ conflict = false, workspace = false }: { conflict?: boolean; workspace?: boolean } = {}) {
  let revisions: { templateId: string; version: typeof firstVersion | typeof secondVersion; template: typeof first }[] = conflict
    ? [{ templateId: "stable-template", version: firstVersion, template: first }]
    : [];
  vi.mocked(invoke).mockImplementation(async (command, input) => {
    switch (command) {
      case "list_repository_deliveries":
      case "list_repository_markdown_revisions":
        return [];
      case "list_conversation_templates": {
        const head = revisions[0];
        if (!head) return [];
        const { templateId, version, template } = head;
        return [{ templateId, version, name: template.name }];
      }
      case "conversation_models":
        return input && "provider" in input && input.provider === "codex" ? [{ option, efforts: ["high"], defaultEffort: null }] : [];
      case "conversation_template_profile": return {
        cliVersion: "0.169.0-alpha.11.1", transport: "codex_app_server",
        toolScopes: [{ mode: "read_only", blocker: null }, { mode: "approved_write", blocker: "Workspace writes are unavailable for this runner." }],
        capabilities: [{ capability: "workspace_write", status: "blocked" }],
      };
      case "conversation_template_revision": {
        const reference = input as { templateId: string; revision: string };
        const revision = revisions.find(({ templateId, version }) => templateId === reference.templateId && version.revision === reference.revision);
        if (!revision) throw new Error("Unknown template revision");
        return { revision, history: revisions.map(({ version }) => version) };
      }
      case "save_conversation_template": {
        const args = input as { projectId: string; base: { expectedRevision: string } | null; template: typeof first };
        const version = args.base ? secondVersion : firstVersion;
        const revision = { templateId: "stable-template", version, template: conflict
          ? { ...first, instructions: "Edited in a different window." }
          : args.template };
        revisions = [revision, ...revisions];
        if (conflict) return { status: "conflict", current: { templateId: revision.templateId, version, name: first.name } };
        return { status: "saved", detail: { revision, history: revisions.map(({ version }) => version) } };
      }
      default: throw new Error(command);
    }
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  function LiveWorkspace() {
    const [target, setTarget] = useState<string>();
    return <PipelinesWorkspace checkout={checkout} target={target} simulation={null}
      openWorkspace={(_, destination) => setTarget(destination)} startSimulation={vi.fn()}
      openActivity={vi.fn()} openChanges={vi.fn()} />;
  }
  render(<QueryClientProvider client={client}>
    {workspace ? <LiveWorkspace /> : <ConversationTemplateEditor checkout={checkout} />}
  </QueryClientProvider>);
  return userEvent.setup();
}

it("saves all template fields with a qualified model and reopens the exact prior revision", async () => {
  const user = setup({ workspace: true });
  await user.click(screen.getByRole("button", { name: "Roles & templates" }));
  await user.click(screen.getByRole("button", { name: "New conversation template" }));
  await user.type(screen.getByRole("textbox", { name: "Name" }), first.name);
  await user.selectOptions(screen.getByRole("combobox", { name: "Role" }), first.role);
  await user.click(await screen.findByRole("button", { name: /^Default CLI and model · / }));
  const row = await screen.findByRole("menuitemradio", { name: option.name });
  await user.hover(row);
  expect(within(await screen.findByLabelText(`Ratings for ${option.name}`)).getByText("API reference $1.25 / 1M tokens")).toBeTruthy();
  await user.click(row);
  await user.selectOptions(screen.getByRole("combobox", { name: "Effort" }), first.effort);
  await user.type(screen.getByRole("textbox", { name: "Instructions" }), first.instructions);
  await user.type(screen.getByRole("textbox", { name: "Completion contract" }), first.completionContract);
  await user.selectOptions(screen.getByRole("combobox", { name: "Review policy" }), first.reviewPolicy);
  await user.click(await screen.findByText("Capability matrix"));
  expect(screen.getByText("workspace write · blocked")).toBeTruthy();
  expect(screen.getByRole<HTMLOptionElement>("option", { name: "Approved worktree write (unavailable)" }).disabled).toBe(true);
  await user.click(screen.getByRole("button", { name: "Save template revision" }));
  expect(invoke).toHaveBeenCalledWith("save_conversation_template", { ...scope, base: null, template: first });
  await screen.findByRole("heading", { name: "Edit conversation template" });
  await user.clear(screen.getByRole("textbox", { name: "Instructions" }));
  await user.type(screen.getByRole("textbox", { name: "Instructions" }), "Read the later evidence.");
  await user.click(screen.getByRole("button", { name: "Save template revision" }));
  expect(invoke).toHaveBeenCalledWith("save_conversation_template", {
    ...scope, base: { templateId: "stable-template", expectedRevision: firstVersion.revision },
    template: { ...first, instructions: "Read the later evidence." },
  });
  await waitFor(() => expect(screen.getByRole<HTMLSelectElement>("combobox", { name: "Saved revision" }).value).toBe(secondVersion.revision));
  await user.selectOptions(screen.getByRole("combobox", { name: "Saved revision" }), firstVersion.revision);
  await screen.findByRole("heading", { name: "Revision 1" });
  expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Instructions" }).value).toBe(first.instructions);
  expect(screen.queryByRole("button", { name: "Save template revision" })).toBeNull();
});

it("keeps competing local edits while offering to open the newly saved head", async () => {
  const user = setup({ conflict: true });
  const instructions = await screen.findByRole<HTMLTextAreaElement>("textbox", { name: "Instructions" });
  await user.clear(instructions);
  await user.type(instructions, "Local draft instructions.");
  const save = screen.getByRole<HTMLButtonElement>("button", { name: "Save template revision" });
  await waitFor(() => expect(save.disabled).toBe(false));
  await user.click(save);
  expect(await screen.findByText("Revision 2 was saved elsewhere. Your edits are still here.")).toBeTruthy();
  expect(instructions.value).toBe("Local draft instructions.");
  await user.click(screen.getByRole("button", { name: "Discard edits and open latest revision" }));
  await waitFor(() => expect(screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Instructions" }).value).toBe("Edited in a different window."));
  expect(screen.getByRole("heading", { name: "Edit conversation template" })).toBeTruthy();
});
