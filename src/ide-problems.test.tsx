// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { IdeProblems, type FileProblem } from "@/ide-problems";

afterEach(cleanup);

it("combines exact file and severity filters while keeping the related diagnostics copy complete", async () => {
  const sourceErrors: FileProblem[] = [
    { path: "src/one/index.ts", line: 1, column: 2, label: "Error", message: "First error" },
    { path: "src/one/index.ts", line: 2, column: 3, label: "Warning", message: "First warning" },
    { path: "src/two/index.ts", line: 1, column: 2, label: "Error", message: "Other error" },
  ];
  const copy = vi.fn();
  const props = { copy, simulated: false, open: vi.fn(), reveal: vi.fn(), quickFix: vi.fn() };
  const { rerender } = render(<IdeProblems issues={sourceErrors} {...props} />);
  const user = userEvent.setup();

  fireEvent.contextMenu(screen.getByRole("button", { name: /First error/ }));
  await user.click(screen.getByRole("menuitem", { name: "Filter to this file" }));
  expect(screen.getByRole("status").textContent).toBe("2 of 3 diagnostics");
  expect(screen.queryByRole("button", { name: /Other error/ })).toBeNull();

  fireEvent.contextMenu(screen.getByRole("button", { name: /First error/ }));
  await user.click(screen.getByRole("menuitem", { name: "Filter to error diagnostics" }));
  expect(screen.getByRole("status").textContent).toBe("1 of 3 diagnostics");
  expect(screen.queryByRole("button", { name: /First warning/ })).toBeNull();
  fireEvent.contextMenu(screen.getByRole("button", { name: /First error/ }));
  await user.click(screen.getByRole("menuitem", { name: "Copy diagnostics in this file" }));
  expect(copy).toHaveBeenCalledWith("src/one/index.ts:1:2 Error: First error\nsrc/one/index.ts:2:3 Warning: First warning", "Copied file diagnostics");

  rerender(<IdeProblems issues={sourceErrors.slice(1)} {...props} />);
  expect(screen.getByText("No diagnostics match your filters.")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Clear severity filter (Error)" }));
  expect(screen.getByRole("button", { name: /First warning/ })).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "Clear file filter (index.ts)" }));
  expect(screen.getByRole("button", { name: /Other error/ })).toBeTruthy();
});
