// @vitest-environment jsdom
import { invoke } from "@tauri-apps/api/core";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { NotePropertyEditor } from "@/note-property-editor";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("submits a committed YAML property edit through the active note buffer", async () => {
  const source = "---\nstatus: ready # review\n---\n# Release\n";
  const edited = source.replace("ready", "done");
  const update = vi.fn();
  vi.mocked(invoke).mockResolvedValueOnce(edited);
  render(<NotePropertyEditor name="status" value="ready" read={() => source} update={update} />);

  fireEvent.change(screen.getByRole("textbox", { name: "status" }), { target: { value: "done" } });
  expect(invoke).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Apply" }));

  expect(invoke).toHaveBeenCalledWith("replace_note_property", { markdown: source, key: "status", value: "done" });
  await waitFor(() => expect(update).toHaveBeenCalledWith(edited));
});

it("does not let a delayed property response replace newer Source edits", async () => {
  let source = "---\nstatus: ready\n---\n# Release\n";
  let respond!: (markdown: string) => void;
  vi.mocked(invoke).mockImplementationOnce(() => new Promise<string>((resolve) => { respond = resolve; }));
  const update = vi.fn();
  render(<NotePropertyEditor name="status" value="ready" read={() => source} update={update} />);

  const input = screen.getByRole("textbox", { name: "status" });
  fireEvent.change(input, { target: { value: "done" } });
  fireEvent.click(screen.getByRole("button", { name: "Apply" }));
  const edited = source.replace("ready", "done");
  source += "new text\n";
  respond(edited);

  expect((await screen.findByRole("alert")).textContent).toContain("The note changed");
  expect(update).not.toHaveBeenCalled();
  expect((input as HTMLInputElement).value).toBe("done");
});
