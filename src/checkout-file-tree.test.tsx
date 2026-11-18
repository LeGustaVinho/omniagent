// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { act, cleanup, fireEvent, render, renderHook, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { moveCheckoutFavorites, reconcileFavoriteItems, saveCheckoutFavoriteGroup, setCheckoutFavorites, useCheckoutFavorites } from "@/checkout-favorites";
import { FileTree, type FileTreeTarget } from "@/ide-file-workspace";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); localStorage.clear(); vi.resetAllMocks(); });

const root = () => screen.getByRole("button", { name: "Checkout root" });

it("reveals the active file once without enabling automatic reveal", async () => {
  const client = new QueryClient();
  const tree = (active: string) => <QueryClientProvider client={client}>
    <FileTree paths={["src/first.txt", "docs/other.md"]} folders={["src", "docs"]} active={active} onOpen={vi.fn()}
      actions={() => []} checkoutId="reveal-worktree" workspace="ide" simulated={false} />
  </QueryClientProvider>;
  const { rerender } = render(tree("src/first.txt"));
  expect(screen.queryByRole("button", { name: "first.txt" })).toBeNull();
  fireEvent.contextMenu(root());
  await userEvent.click(await screen.findByRole("menuitem", { name: "Reveal active file" }));
  expect((await screen.findByRole("button", { name: "first.txt" })).getAttribute("aria-current")).toBe("page");

  rerender(tree("docs/other.md"));
  expect(screen.queryByRole("button", { name: "other.md" })).toBeNull();
  fireEvent.contextMenu(root());
  expect(await screen.findByRole("menuitem", { name: "Auto-reveal active file" })).toBeTruthy();
});

it("loads Git-ignored directories lazily from the blank-area context menu", async () => {
  vi.mocked(invoke).mockImplementation(async (_command, args) => {
    switch ((args as { folder: string | null }).folder) {
      case null: return { folders: ["cache"], files: ["records/private.log"] };
      case "cache": return { folders: ["cache/reports"], files: ["cache/entry.txt"] };
      case "cache/reports": return { folders: [], files: ["cache/reports/older.txt"] };
      default: throw new Error("Unexpected ignored folder request");
    }
  });
  const refresh = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const { container } = render(<QueryClientProvider client={client}><FileTree paths={["reading.md"]} onOpen={vi.fn()}
    actions={(item) => item.kind === "root" ? [[{ label: "Refresh tree", run: refresh }]] : []}
    workspace="ide" simulated={false} checkoutId="worktree-for-ignored-items" /></QueryClientProvider>);
  const user = userEvent.setup();
  expect(invoke).not.toHaveBeenCalled();
  fireEvent.contextMenu(container.querySelector(".file-tree-blank")!);
  await user.click(await screen.findByRole("menuitem", { name: "Refresh tree" }));
  expect(refresh).toHaveBeenCalledOnce();
  fireEvent.contextMenu(container.querySelector(".file-tree-blank")!);
  await user.click(await screen.findByRole("menuitem", { name: "Show Git-ignored files" }));
  const cache = await screen.findByRole("button", { name: "cache" });
  expect(cache.getAttribute("data-git-ignored")).toBe("true");
  expect(invoke).toHaveBeenCalledExactlyOnceWith("list_checkout_ignored_items", { checkoutId: "worktree-for-ignored-items", folder: null });
  await user.click(cache);
  expect(await screen.findByRole("button", { name: "entry.txt" })).toBeTruthy();
  expect(invoke).toHaveBeenLastCalledWith("list_checkout_ignored_items", { checkoutId: "worktree-for-ignored-items", folder: "cache" });
  expect(invoke).toHaveBeenCalledTimes(2);
  await user.click(screen.getByRole("button", { name: "reports" }));
  expect(await screen.findByRole("button", { name: "older.txt" })).toBeTruthy();
  expect(invoke).toHaveBeenLastCalledWith("list_checkout_ignored_items", { checkoutId: "worktree-for-ignored-items", folder: "cache/reports" });
  fireEvent.contextMenu(root());
  await user.click(await screen.findByRole("menuitem", { name: "Hide Git-ignored files" }));
  expect(screen.queryByRole("button", { name: "cache" })).toBeNull();
});

it("persists tree preferences, reveals the active file, and keeps favorites scoped to the worktree", async () => {
  const actions = (_item: FileTreeTarget) => [];
  const open = vi.fn();
  function fixture(checkoutId = "favorites-worktree", active?: string) {
    return <QueryClientProvider client={new QueryClient()}><FileTree paths={["a.txt", "B.md", ".secret", "docs/note.md"]}
      folders={["docs"]} active={active} onOpen={open} actions={actions} checkoutId={checkoutId} workspace="ide" simulated={false} /></QueryClientProvider>;
  }
  const user = userEvent.setup();
  const mounted = render(fixture("favorites-worktree", "docs/note.md"));
  const rootMenu = async () => {
    fireEvent.contextMenu(root());
    return within(await screen.findByRole("menu", { name: "Checkout root" }));
  };
  await user.click((await rootMenu()).getByRole("menuitem", { name: "Hide dotfiles" }));
  expect(screen.queryByRole("button", { name: ".secret" })).toBeNull();
  const sorting = (await rootMenu()).getByRole("menuitem", { name: "Sort by" });
  sorting.focus();
  await user.keyboard("{ArrowRight}");
  await user.click(await screen.findByRole("menuitem", { name: "Name descending" }));
  const visibleFileNames = () => [...mounted.container.querySelectorAll<HTMLButtonElement>("button[data-tree-item]:not([aria-expanded])")]
    .map((item) => item.title);
  expect(visibleFileNames()).toEqual(["B.md", "a.txt"]);
  await user.click((await rootMenu()).getByRole("menuitem", { name: "Auto-reveal active file" }));
  expect((await screen.findByRole("button", { name: "note.md" })).getAttribute("aria-current")).toBe("page");

  fireEvent.click(screen.getByRole("button", { name: "docs" }), { ctrlKey: true });
  fireEvent.click(screen.getByRole("button", { name: "a.txt" }), { ctrlKey: true });
  fireEvent.contextMenu(screen.getByRole("button", { name: "a.txt" }));
  await user.click(await screen.findByRole("menuitem", { name: "Add selected to favorites" }));
  const favorite = () => within(screen.getByRole("region", { name: "Favorites" }));
  expect(favorite().getByRole("button", { name: /^docs/ })).toBeTruthy();
  await user.click(favorite().getByRole("button", { name: /^a\.txt/ }));
  expect(open).toHaveBeenCalledWith("a.txt");

  act(() => reconcileFavoriteItems("favorites-worktree", (path) => path === "a.txt" ? "renamed.txt" : path === "docs" ? null : undefined));
  expect(favorite().queryByRole("button", { name: /^docs/ })).toBeNull();
  expect(favorite().getByRole("button", { name: /^renamed\.txt/ })).toBeTruthy();

  mounted.rerender(fixture("another-worktree"));
  expect(screen.queryByRole("region", { name: "Favorites" })).toBeNull();
  expect(screen.getByRole("button", { name: ".secret" })).toBeTruthy();
  mounted.rerender(fixture());
  expect(favorite().getByRole("button", { name: /^renamed\.txt/ })).toBeTruthy();
  expect(screen.queryByRole("button", { name: ".secret" })).toBeNull();
  expect(visibleFileNames()).toEqual(["docs/note.md", "B.md", "a.txt"]);
});

it("filters Windows attributes separately and reveals a favorite hidden folder without showing its hidden children", async () => {
  const checkoutId = "hidden-items-worktree";
  vi.mocked(invoke).mockImplementation(async (command, args) => {
    if (command !== "list_checkout_windows_hidden_items") throw new Error("Unexpected checkout request");
    switch ((args as { folder: string }).folder) {
      case "": return ["ROOT-SECRET.md"];
      case "reports": return ["reports/private"];
      case "reports/private": return ["reports/private/hidden-child.md"];
      default: throw new Error("Unexpected hidden folder request");
    }
  });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const props = { paths: ["root-secret.md", ".dot.md", "reports/ordinary.md", "reports/private/note.md", "reports/private/hidden-child.md"],
    folders: ["reports", "reports/private"], onOpen: vi.fn(), actions: () => [], checkoutId, workspace: "knowledge" as const, simulated: false };
  const fixture = (overrides: Partial<ComponentProps<typeof FileTree>> = {}) =>
    <QueryClientProvider client={client}><FileTree {...props} {...overrides} /></QueryClientProvider>;
  const mounted = render(fixture());
  const user = userEvent.setup();
  const rootCommand = async (name: string) => {
    fireEvent.contextMenu(root());
    await user.click(await screen.findByRole("menuitem", { name }));
  };

  await user.click(screen.getByRole("button", { name: "reports" }));
  fireEvent.contextMenu(screen.getByRole("button", { name: "private" }));
  await user.click(await screen.findByRole("menuitem", { name: "Add to favorites" }));
  expect(invoke).not.toHaveBeenCalled();

  await rootCommand("Hide Windows hidden items");
  expect(await screen.findByRole("button", { name: "ordinary.md" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "root-secret.md" })).toBeNull();
  expect(screen.queryByRole("button", { name: "private" })).toBeNull();
  expect(screen.getByRole("button", { name: ".dot.md" })).toBeTruthy();
  expect(invoke).not.toHaveBeenCalledWith("list_checkout_windows_hidden_items", { checkoutId, folder: "reports/private" });

  const favorite = within(screen.getByRole("region", { name: "Favorites" }));
  await user.click(favorite.getByRole("button", { name: /^private/ }));
  expect(await screen.findByRole("button", { name: "note.md" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "hidden-child.md" })).toBeNull();
  expect(invoke).toHaveBeenLastCalledWith("list_checkout_windows_hidden_items", { checkoutId, folder: "reports/private" });

  mounted.rerender(fixture({ reveal: { path: "reports/private/hidden-child.md" } }));
  expect(await screen.findByRole("button", { name: "hidden-child.md" })).toBeTruthy();
  await rootCommand("Hide dotfiles");
  expect(screen.queryByRole("button", { name: ".dot.md" })).toBeNull();
  expect(screen.queryByRole("button", { name: "root-secret.md" })).toBeNull();
  await rootCommand("Show Windows hidden items");
  expect(screen.getByRole("button", { name: "root-secret.md" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: ".dot.md" })).toBeNull();
});

it("groups legacy favorites through the shared explorer menus and keeps assignments in the exact worktree", async () => {
  const checkoutId = "grouped worktree";
  const key = `omniagent:checkout-favorites:${encodeURIComponent(checkoutId)}`;
  localStorage.setItem(key, JSON.stringify([
    { kind: "folder", path: "docs" },
    { kind: "file", path: "docs/note.md" },
    { kind: "file", path: "code.ts" },
  ]));
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const onOpen = vi.fn();
  function fixture(workspace: "ide" | "knowledge" = "ide", currentCheckout = checkoutId, folder = "docs") {
    return <QueryClientProvider client={client}><FileTree paths={workspace === "ide" ? [`${folder}/note.md`, "code.ts"] : [`${folder}/note.md`]}
      folders={[folder]} checkoutId={currentCheckout} workspace={workspace} simulated={false} actions={() => []} onOpen={onOpen} /></QueryClientProvider>;
  }
  const mounted = render(fixture());
  const user = userEvent.setup();
  const favorites = () => within(screen.getByRole("region", { name: "Favorites" }));
  const group = (name: string) => within(favorites().getByRole("region", { name: `Favorite group ${name}` }));
  async function createGroup(name: string) {
    fireEvent.contextMenu(root());
    await user.click(await screen.findByRole("menuitem", { name: "New favorite group…" }));
    const dialog = within(await screen.findByRole("dialog", { name: "New favorite group" }));
    await user.type(dialog.getByRole("textbox", { name: "Group name" }), name);
    await user.click(dialog.getByRole("button", { name: "Create group" }));
  }
  async function moveFavorite(button: HTMLElement, name: string) {
    fireEvent.contextMenu(button);
    const groups = await screen.findByRole("menuitem", { name: "Move to favorite group" });
    groups.focus();
    await user.keyboard("{ArrowRight}");
    await user.click(await screen.findByRole("menuitem", { name: `Move to ${name}` }));
  }

  expect(favorites().getByRole("button", { name: /^note\.md/ })).toBeTruthy();
  await createGroup("Reading");
  await createGroup("Current");
  await moveFavorite(favorites().getByRole("button", { name: /^note\.md/ }), "Reading");
  await moveFavorite(group("Reading").getByRole("button", { name: /^note\.md/ }), "Current");
  await moveFavorite(favorites().getByRole("button", { name: /^docs/ }), "Current");
  expect(group("Reading").queryByRole("button", { name: /^note\.md/ })).toBeNull();

  fireEvent.contextMenu(group("Current").getByRole("button", { name: "Current" }));
  await user.click(await screen.findByRole("menuitem", { name: "Rename favorite group…" }));
  const rename = within(await screen.findByRole("dialog", { name: "Rename favorite group" }));
  await user.clear(rename.getByRole("textbox", { name: "Group name" }));
  await user.type(rename.getByRole("textbox", { name: "Group name" }), "Research");
  await user.click(rename.getByRole("button", { name: "Rename group" }));
  expect(group("Research").getByRole("button", { name: /^note\.md/ })).toBeTruthy();

  act(() => reconcileFavoriteItems(checkoutId, (path) => path === "docs" || path.startsWith("docs/") ? `archive${path.slice(4)}` : undefined));
  mounted.rerender(fixture("knowledge", checkoutId, "archive"));
  const note = group("Research").getByRole("button", { name: /^note\.md/ });
  expect(note.title).toBe("archive/note.md");
  expect(group("Research").getByRole("button", { name: /^archive/ }).title).toBe("archive");
  expect(favorites().queryByRole("button", { name: /^code\.ts/ })).toBeNull();
  await user.click(note);
  expect(onOpen).toHaveBeenCalledWith("archive/note.md");
  await user.click(group("Research").getByRole("button", { name: /^archive/ }));
  const physicalNote = await screen.findByRole("button", { name: "note.md" });
  fireEvent.contextMenu(note);
  await user.click(await screen.findByRole("menuitem", { name: "Remove from favorites" }));
  expect(group("Research").queryByRole("button", { name: /^note\.md/ })).toBeNull();
  expect(physicalNote.isConnected).toBe(true);
  expect(invoke).not.toHaveBeenCalled();

  mounted.rerender(fixture("knowledge", "different worktree", "archive"));
  expect(screen.queryByRole("region", { name: "Favorites" })).toBeNull();
  mounted.rerender(fixture("ide", checkoutId, "archive"));
  expect(group("Research").getByRole("button", { name: /^archive/ })).toBeTruthy();
  expect(favorites().getByRole("button", { name: /^code\.ts/ })).toBeTruthy();
});

it("keeps group assignments on repeated adds and when an exact move replaces a stale favorite path", () => {
  const checkout = "grouped-reconciliation";
  const source = { kind: "file", path: "before.md" } as const;
  const destination = { kind: "file", path: "after.md" } as const;
  const state = renderHook(() => useCheckoutFavorites(checkout));
  act(() => {
    setCheckoutFavorites(checkout, [source, destination], false);
    saveCheckoutFavoriteGroup(checkout, { name: "Active", paths: [source.path] });
    saveCheckoutFavoriteGroup(checkout, { name: "Old", paths: [destination.path] });
    setCheckoutFavorites(checkout, [source], false);
  });
  const active = state.result.current.groups.find((group) => group.name === "Active")!;
  expect(() => saveCheckoutFavoriteGroup(checkout, { name: " active " })).toThrow(/already exists/);
  expect(state.result.current.favorites.find((favorite) => favorite.path === source.path)?.groupId).toBe(active.id);
  act(() => reconcileFavoriteItems(checkout, (path) => path === source.path ? destination.path : undefined));
  expect(state.result.current.favorites).toEqual([{ ...destination, groupId: active.id }]);
  act(() => moveCheckoutFavorites(checkout, [destination.path]));
  expect(state.result.current.favorites).toEqual([destination]);
});

it("reports a Windows-attribute lookup failure while keeping the known checkout files usable", async () => {
  vi.mocked(invoke).mockRejectedValue(new Error("Permission denied"));
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const open = vi.fn();
  render(<QueryClientProvider client={client}><FileTree paths={["known.md"]} onOpen={open} actions={() => []}
    checkoutId="unreadable-hidden-items-worktree" workspace="ide" simulated={false} /></QueryClientProvider>);
  const user = userEvent.setup();
  fireEvent.contextMenu(root());
  await user.click(await screen.findByRole("menuitem", { name: "Hide Windows hidden items" }));
  expect((await screen.findByRole("alert")).textContent).toContain("Permission denied");
  await user.click(screen.getByRole("button", { name: "known.md" }));
  expect(open).toHaveBeenCalledWith("known.md");
});

it("keeps an anchor favorite separate from its note and other anchor occurrences in a group", () => {
  const checkout = "favorite-anchor-worktree";
  const file = { path: "notes/outline.md", kind: "file" } as const;
  const heading = { path: file.path, kind: "heading", anchor: "scope", label: "Scope", occurrence: 0 } as const;
  const next = { ...heading, occurrence: 1 };
  const block = { path: file.path, kind: "block", anchor: "^decision", label: "^decision", occurrence: 0 } as const;
  const state = renderHook(() => useCheckoutFavorites(checkout));
  act(() => {
    setCheckoutFavorites(checkout, [file, heading, next, block], false);
    saveCheckoutFavoriteGroup(checkout, { name: "Second scope", items: [next] });
  });
  const group = state.result.current.groups[0]!;
  expect(state.result.current.favorites).toEqual([file, heading, { ...next, groupId: group.id }, block]);
  act(() => reconcileFavoriteItems(checkout, (path) => path === file.path ? "notes/moved.md" : undefined));
  const moved = "notes/moved.md";
  act(() => setCheckoutFavorites(checkout, [{ ...next, path: moved }], true));
  expect(state.result.current.favorites).toEqual([file, heading, block].map((item) => ({ ...item, path: moved })));
});
