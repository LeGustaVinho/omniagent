// @vitest-environment jsdom

import { useState } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ComposerCustomizationPicker, hasComposerSkills, useComposerCustomizations, type ComposerCustomization } from "@/conversation-customization-picker";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const codex: ModelSelection = { provider: "codex", profileId: "codex-profile", modelId: "luna" };
const claude: ModelSelection = { provider: "claude", profileId: "claude-profile", modelId: "haiku" };
const exportMarker = `@skill:${"b".repeat(64)}:${"d".repeat(64)}`;
const local: ComposerCustomization = {
  marker: `@skill:${"a".repeat(64)}:${"c".repeat(64)}`, availability: "filesystem",
  skill: { id: "a".repeat(64), provider: "codex", name: "format", path: "C:/workspace/.agents/skills/format/SKILL.md", namespace: null, revision: "r1" },
};
const exported: ComposerCustomization = {
  marker: exportMarker, availability: "provider",
  skill: { ...local.skill, id: "b".repeat(64), path: "C:/plugins/cache/skills/format/SKILL.md", namespace: "fixture@local", revision: "r2" },
};

function Composer({ draft: initialDraft = "", send }: { draft?: string; send: (text: string) => void }) {
  const [draft, setDraft] = useState(initialDraft);
  const [cursor, setCursor] = useState(initialDraft.length);
  const [selection, setSelection] = useState(codex);
  const picker = useComposerCustomizations({ conversationId: "current-conversation", selection, mode: "read_only", effort: null, draft, cursor, enabled: true });
  function pick(option: ComposerCustomization) {
    const completion = picker.pick(option);
    setDraft(completion.text);
    setCursor(completion.cursor);
  }
  return <form onSubmit={(event) => { event.preventDefault(); send(draft); }}>
    <ComposerCustomizationPicker picker={picker} onPick={pick} onRemove={(token) => setDraft(picker.remove(token))} />
    <label>Draft<textarea value={draft} onChange={(event) => {
      setDraft(event.currentTarget.value);
      setCursor(event.currentTarget.selectionStart);
    }} onSelect={(event) => setCursor(event.currentTarget.selectionStart)} onKeyDown={(event) => {
      const selected = picker.options[picker.highlighted];
      if (!picker.active || !selected) return;
      if (event.key === "ArrowDown") { event.preventDefault(); picker.move(1); }
      if (event.key === "Enter") { event.preventDefault(); pick(selected); }
    }} /></label>
    <button type="button" onClick={() => setSelection(claude)}>Switch profile</button>
    <button type="submit">Send</button>
  </form>;
}

function renderComposer(draft?: string) {
  const send = vi.fn();
  const client = new QueryClient();
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    expect(command).toBe("composer_customizations");
    expect(args).toMatchObject({ conversationId: "current-conversation", effort: null, mode: "read_only" });
    return args && "selection" in args && (args.selection as ModelSelection).provider === "codex" ? [local, exported] : [];
  });
  render(<QueryClientProvider client={client}><Composer draft={draft} send={send} /></QueryClientProvider>);
  return { user: userEvent.setup(), send, composer: screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Draft" }) };
}

it("completes a scoped slash invocation and selects the exact namespaced plugin export", async () => {
  const { user, send, composer } = renderComposer();
  await user.type(composer, "/for");
  const list = within(await screen.findByRole("listbox", { name: "Skill and plugin invocations" }));
  expect(list.getByRole("option", { name: /workspace.*Discovered/ })).toBeTruthy();
  expect(list.getByRole("option", { name: /fixture@local.*Reported by CLI/ })).toBeTruthy();
  await user.keyboard("{ArrowDown}{Enter}");
  expect(composer.value.trim()).toBe(exportMarker);
  expect(send).not.toHaveBeenCalled();
  expect(hasComposerSkills(composer.value)).toBe(true);
  const pinned = within(screen.getByRole("list", { name: "Selected skills and plugin exports" }));
  expect(pinned.getByText("fixture@local · format")).toBeTruthy();
  expect(pinned.getByText(exported.skill.path)).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Send" }));
  expect(send).toHaveBeenCalledWith(`${exportMarker} `);
  expect(invoke).toHaveBeenCalledWith("composer_customizations", {
    conversationId: "current-conversation", selection: codex, effort: null, mode: "read_only",
  });
});

it("restores a pinned skill, drops availability after a profile change and lets the user remove it", async () => {
  const { user, composer } = renderComposer(`Check the input. ${exportMarker} `);
  expect(await screen.findByText("fixture@local · format")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Switch profile" }));
  expect(await screen.findByText("Skill selection unavailable for this profile")).toBeTruthy();
  expect(screen.queryByText("fixture@local · format")).toBeNull();
  await user.click(screen.getByRole("button", { name: "Remove unavailable skill" }));
  expect(composer.value.trim()).toBe("Check the input.");
  expect(hasComposerSkills(composer.value)).toBe(false);
  await user.type(composer, "@conversation");
  expect(screen.queryByRole("listbox", { name: "Skill and plugin invocations" })).toBeNull();
});
