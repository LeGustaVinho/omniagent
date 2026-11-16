// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { RepositoryMarkdownPicker } from "@/repository-markdown-picker";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

it("selects a new Markdown source and reopens an older saved revision by checkout and ID", async () => {
  const checkoutId = "worktree-1";
  const old = { id: "old", relativePath: "notes/plan.md", sha256: "a".repeat(64) };
  const renamed = { id: "new", relativePath: "notes/renamed.md", sha256: "b".repeat(64) };
  let revisions = [old];
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    expect(args).toMatchObject({ checkoutId });
    if (command === "list_repository_markdown_revisions") return revisions;
    if (command === "choose_repository_markdown") {
      revisions = [renamed, old];
      return { revision: renamed, markdown: "# Current plan" };
    }
    if (command === "read_repository_markdown" && args && "revisionId" in args && args.revisionId === old.id) {
      return { revision: old, markdown: "# Original plan" };
    }
    throw new Error(`Unexpected command ${command}`);
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const user = userEvent.setup();
  render(<QueryClientProvider client={client}><RepositoryMarkdownPicker checkoutId={checkoutId} /></QueryClientProvider>);
  const select = await screen.findByRole("combobox", { name: "Saved source revision" });
  await user.click(screen.getByRole("button", { name: "Select Markdown file" }));
  await screen.findByRole("option", { name: /notes\/renamed\.md/ });
  expect((select as HTMLSelectElement).value).toBe(renamed.id);
  await user.click(screen.getByText("Read saved Markdown"));
  expect(await screen.findByText("# Current plan")).toBeTruthy();

  await user.selectOptions(select, old.id);
  expect(await screen.findByText("# Original plan")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("read_repository_markdown", { checkoutId, revisionId: old.id });
});
