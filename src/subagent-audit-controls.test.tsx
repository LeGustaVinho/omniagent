// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AttemptSubagentAudit, ConversationSubagentAuditControl } from "@/subagent-audit-controls";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("shows the inherited conversation policy and fields required for complete audit", async () => {
  vi.mocked(invoke).mockResolvedValue({ projectDefault: "complete", conversationOverride: null });
  render(<QueryClientProvider client={new QueryClient()}>
    <ConversationSubagentAuditControl conversationId="conversation" open />
  </QueryClientProvider>);
  expect(await screen.findByText("Require complete audit evidence · Inherited from the project")).toBeTruthy();
  expect(screen.getByText("Identity, Prompt, Context provenance, Response")).toBeTruthy();
  expect(invoke).toHaveBeenCalledWith("conversation_subagent_audit", { conversationId: "conversation" });
});

it("shows the saved delegation decision and observed evidence gaps for the selected attempt", async () => {
  vi.mocked(invoke).mockResolvedValue({
    effectivePolicy: "complete", policySource: "conversation_override", policyConversationId: "root",
    requiredFields: ["identity", "prompt", "context_provenance", "response"],
    route: { kind: "managed", reason: "native_evidence_unavailable" },
  });
  render(<QueryClientProvider client={new QueryClient()}>
    <AttemptSubagentAudit conversationId="child" runId="attempt" live={false} gaps={[{
      source: "native", affectedChildren: 1, unidentified: 2, missing: ["identity", "response"],
      nativeDelegationDetected: true,
    }]} />
  </QueryClientProvider>);
  const audit = within(screen.getByRole("region", { name: "Subagent audit" }));
  expect(await audit.findByText("Delegating conversation override")).toBeTruthy();
  expect(audit.getByText("Managed delegation required. The selected CLI scope lacked a current native audit qualification.")).toBeTruthy();
  expect(audit.getByText(/Missing Identity, Response · 1 identified with gaps · 2 without confirmed identity/)).toBeTruthy();
  expect(audit.getByRole("alert").textContent).toContain("native delegation was disabled");
  expect(invoke).toHaveBeenCalledWith("attempt_subagent_audit", { conversationId: "child", runId: "attempt" });
});
