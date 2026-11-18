// @vitest-environment jsdom
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
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

afterEach(() => {
  cleanup();
  localStorage.removeItem("omniagent:composer-model-order");
});

it("shows the same model ratings in the compact menu and selects by keyboard", async () => {
  function Form() {
    const [selected, onChange] = useState<ModelSelection | null>(null);
    return <ModelPicker label="Model" options={options} selection={{ mode: "single", selected, onChange }} />;
  }
  const user = userEvent.setup();
  render(<Form />);
  await user.tab();
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "Model · Choose model" }));
  await user.keyboard("{Enter}");
  const menu = await screen.findByRole("menu", { name: "Model" });
  const radio = within(menu).getByRole("menuitemradio", { name: "GPT‑6 Luna" });
  await waitFor(() => expect(document.activeElement).toBe(radio));
  expect(within(menu).getByText("16.9/100")).toBeDefined();
  expect(within(menu).getByText("0/100")).toBeDefined();
  expect(within(menu).getByText("API reference $0.20 / 1M tokens")).toBeDefined();
  expect((within(menu).getByRole("meter", { name: "Price score" }) as HTMLMeterElement).value).toBe(0);
  await user.keyboard("{Enter}");
  expect(screen.queryByRole("menu")).toBeNull();
  expect(screen.getByRole("button", { name: "Model · GPT‑6 Luna" })).toBeDefined();
});

it("toggles models without closing the multi-select menu or accepting unavailable models", async () => {
  function Form() {
    const [selected, onChange] = useState<ModelSelection[]>([]);
    return <ModelPicker label="Models" options={options} selection={{ mode: "multiple", selected, onChange }} />;
  }
  const user = userEvent.setup();
  render(<Form />);
  await user.click(screen.getByRole("button", { name: "Models · Choose models" }));
  const menu = await screen.findByRole("menu", { name: "Models" });
  const lunaBox = within(menu).getByRole("menuitemcheckbox", { name: "GPT‑6 Luna" });
  const otherBox = within(menu).getByRole("menuitemcheckbox", { name: "Other model" });
  const disabled = within(menu).getByRole("menuitemcheckbox", { name: /GPT‑6 Luna \(work profile\)/ });
  await user.click(lunaBox);
  await user.click(otherBox);
  expect(lunaBox.getAttribute("aria-checked")).toBe("true");
  expect(otherBox.getAttribute("aria-checked")).toBe("true");
  expect(disabled.getAttribute("aria-disabled")).toBe("true");
  expect(within(menu).getByText("Unrated")).toBeDefined();
  await user.click(lunaBox);
  expect(lunaBox.getAttribute("aria-checked")).toBe("false");
  expect(otherBox.getAttribute("aria-checked")).toBe("true");
  await user.keyboard("{Escape}");
  expect(screen.queryByRole("menu")).toBeNull();
  expect(screen.getByRole("button", { name: "Models · 1 selected" })).toBeDefined();
});

it("sorts one catalog across services and preserves the exact model selection and order preference", async () => {
  const claude: ModelOption = { ...luna, name: "Claude Sonnet", selection: { provider: "claude", profileId: "work", modelId: "sonnet" },
    rating: { intelligenceTenths: 730, apiReferenceCents: 230, priceScore: 40 } };
  const gemini: ModelOption = { ...luna, name: "Gemini Pro", selection: { provider: "antigravity", profileId: "personal", modelId: "pro" },
    rating: { intelligenceTenths: 810, apiReferenceCents: 180, priceScore: 70 } };
  const catalog = [unknown, luna, claude, gemini];
  function Form() {
    const [selected, onChange] = useState<ModelSelection | null>(null);
    return <ModelPicker label="Model" options={catalog} combinedCatalog selection={{ mode: "single", selected, onChange }} />;
  }
  const user = userEvent.setup();
  render(<Form />);
  await user.click(screen.getByRole("button", { name: "Model · Choose model" }));
  const menu = await screen.findByRole("menu", { name: "Model" });
  const list = within(menu.querySelector<HTMLElement>(".model-picker-list")!);
  const names = () => list.getAllByRole("menuitemradio").map((row) => row.getAttribute("aria-label"));
  expect(names()).toEqual(["Gemini Pro", "Claude Sonnet", "GPT‑6 Luna", "Other model"]);
  expect(within(menu).getByText("Claude Code")).toBeTruthy();
  expect(within(menu).getByText("Gemini · Antigravity")).toBeTruthy();
  await user.click(within(menu).getByRole("menuitemradio", { name: "Price, lowest first" }));
  expect(names()).toEqual(["GPT‑6 Luna", "Gemini Pro", "Claude Sonnet", "Other model"]);
  await user.click(within(menu).getByRole("menuitemradio", { name: "Gemini Pro" }));
  await user.click(screen.getByRole("button", { name: "Model · Gemini Pro" }));
  expect(screen.getByRole("menuitemradio", { name: "Price, lowest first" }).getAttribute("aria-checked")).toBe("true");
  expect(screen.getByRole("menuitemradio", { name: "Gemini Pro" }).getAttribute("aria-checked")).toBe("true");
});

it("sorts and displays a confirmed price while intelligence is pending", async () => {
  const sol: ModelOption = {
    ...luna, selection: { ...luna.selection, modelId: "gpt-6.2-sol" }, name: "GPT-6.2 Sol",
    rating: { intelligenceTenths: null, apiReferenceCents: 400, priceScore: 65 },
  };
  render(<ModelPicker label="Model" options={[unknown, luna, sol]} combinedCatalog
    selection={{ mode: "single", selected: sol.selection, onChange: () => undefined }} />);
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Model · GPT-6.2 Sol" }));
  expect(screen.getByText("Intelligence pending")).toBeDefined();
  expect(screen.getByText("API reference $4.00 / 1M tokens")).toBeDefined();
  await user.click(screen.getByRole("menuitemradio", { name: "Price, lowest first" }));
  const list = within(document.querySelector<HTMLElement>(".model-picker-list")!);
  expect(list.getAllByRole("menuitemradio").map((row) => row.getAttribute("aria-label")))
    .toEqual(["GPT‑6 Luna", "GPT-6.2 Sol", "Other model"]);
});
