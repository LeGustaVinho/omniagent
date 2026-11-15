// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { afterEach, expect, it } from "vitest";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelSelection } from "@/bindings/ModelSelection";
import { ModelPicker } from "./model-picker";

const luna: ModelOption = {
  selection: { provider: "codex", profileId: "personal", modelId: "gpt-6-luna" },
  name: "GPT‑6 Luna",
  rating: { intelligenceTenths: 169, apiReferenceCents: 20, priceScore: 0 },
  selectable: true, unavailableReason: null, catalogVersion: 1,
};
const unknown: ModelOption = {
  ...luna, name: "Other model",
  selection: { ...luna.selection, modelId: "unknown" },
  rating: null,
};
const unavailable: ModelOption = {
  ...luna, name: "GPT‑6 Luna (work profile)", selectable: false, unavailableReason: "Disabled by profile",
  selection: { ...luna.selection, profileId: "work" },
};
const options = [luna, unknown, unavailable];

afterEach(cleanup);

it("keeps rate and a zero price score visible after keyboard selection", async () => {
  function Form() {
    const [selected, onChange] = useState<ModelSelection | null>(null);
    return <ModelPicker label="Model" options={options} selection={{ mode: "single", selected, onChange }} />;
  }
  const user = userEvent.setup();
  const { container } = render(<Form />);
  await user.tab();
  expect(document.activeElement?.tagName).toBe("SUMMARY");
  await user.keyboard("{Enter}");
  // jsdom does not implement the summary's Enter default action.
  const disclosure = container.querySelector("details")!;
  if (!disclosure.open) await user.click(disclosure.querySelector("summary")!);
  const radio = screen.getByRole("radio", { name: "GPT‑6 Luna" });
  await user.tab();
  expect(document.activeElement).toBe(radio);
  await user.keyboard(" ");
  expect((radio as HTMLInputElement).checked).toBe(true);
  const summary = within(disclosure.querySelector("summary")!);
  expect(summary.getByText("16.9/100")).toBeDefined();
  expect(summary.getByText("0/100")).toBeDefined();
  expect(summary.getByText("API reference $0.20 / 1M tokens")).toBeDefined();
  expect((summary.getByRole("meter", { name: "Price score" }) as HTMLMeterElement).value).toBe(0);
});

it("selects available options independently in multi-select mode", async () => {
  function Form() {
    const [selected, onChange] = useState<ModelSelection[]>([]);
    return <ModelPicker label="Models" options={options} selection={{ mode: "multiple", selected, onChange }} />;
  }
  const user = userEvent.setup();
  const { container } = render(<Form />);
  await user.click(container.querySelector("summary")!);
  const lunaBox = screen.getByRole("checkbox", { name: "GPT‑6 Luna" }) as HTMLInputElement;
  const otherBox = screen.getByRole("checkbox", { name: "Other model" }) as HTMLInputElement;
  const disabled = screen.getByRole("checkbox", { name: "GPT‑6 Luna (work profile)" }) as HTMLInputElement;
  await user.click(lunaBox);
  await user.click(otherBox);
  expect(lunaBox.checked && otherBox.checked).toBe(true);
  expect(disabled.disabled && !disabled.checked).toBe(true);
  expect(within(container.querySelector("summary")!).getByText("Unrated")).toBeDefined();
  await user.click(lunaBox);
  expect(!lunaBox.checked && otherBox.checked).toBe(true);
});
