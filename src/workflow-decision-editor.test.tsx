// @vitest-environment jsdom

import { useState } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { DecisionNodeFields, defaultDecisionConfiguration, type WorkflowDecisionConfiguration } from "@/workflow-decision-editor";

afterEach(cleanup);

it("authors typed fields and choice IDs directly in the node inspector", async () => {
  const save = vi.fn<(configuration: WorkflowDecisionConfiguration) => void>();
  function Editor() {
    const [draft, update] = useState(defaultDecisionConfiguration("request_input"));
    return <DecisionNodeFields configuration={draft} onChange={(configuration) => { update(configuration); save(configuration); }} />;
  }
  const user = userEvent.setup();
  render(<Editor />);

  await user.clear(screen.getByLabelText("Question message"));
  await user.type(screen.getByLabelText("Question message"), "Choose the review checks.");
  await user.selectOptions(screen.getByLabelText("Field 1 type"), "choice");
  await user.click(screen.getByLabelText("Allow multiple choices"));
  await user.clear(screen.getByLabelText("Option 1 ID"));
  await user.type(screen.getByLabelText("Option 1 ID"), "build");
  await user.clear(screen.getByLabelText("Option 1 label"));
  await user.type(screen.getByLabelText("Option 1 label"), "Build binaries");
  await user.click(screen.getByRole("button", { name: "Remove option 2" }));

  await user.click(screen.getByRole("button", { name: "Add field" }));
  await user.selectOptions(screen.getByLabelText("Field 2 type"), "number");
  await user.click(screen.getByLabelText("Whole numbers only"));

  expect(save).toHaveBeenLastCalledWith({
    kind: "request_input",
    message: "Choose the review checks.",
    fields: [
      { id: "answer", label: "Answer", required: true, input: { kind: "choice", multiple: true, options: [{ id: "build", label: "Build binaries" }] } },
      { id: "field-2", label: "Answer 2", required: true, input: { kind: "number", integer: true } },
    ],
  });
});
