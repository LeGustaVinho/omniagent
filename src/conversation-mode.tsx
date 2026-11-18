import { Map, Pencil, ShieldCheck, Target } from "lucide-react";
import type { SessionMode } from "@/bindings/SessionMode";
import { ControlTooltip } from "@/components/ui/control-tooltip";

export type ConversationMode = SessionMode | "goal";

const modeDescriptions: Record<ConversationMode, string> = {
  read_only: "Inspect the repository and ask questions.",
  plan: "Develop a plan and review decisions before implementation.",
  approved_write: "Edit files in a managed task worktree. Choose the checkout in Continuation and review any required approvals in Queue.",
  goal: "Set an objective and review its progress over multiple turns.",
};

export function goalCommand(text: string): string | undefined {
  const command = /^\/goal(?:\s+([\s\S]*))?$/i.exec(text.trim());
  return command ? command[1]?.trim() ?? "" : undefined;
}

export const sideQuestionCommandPrefix = "/btw ";

export function sideQuestionCommand(text: string): string | undefined {
  const command = /^\/btw(?:\s+([\s\S]*))?$/i.exec(text.trim());
  return command ? command[1]?.trim() ?? "" : undefined;
}

export function completesSideQuestion(text: string): boolean {
  return /^\/(?:b(?:t(?:w)?)?)?$/i.test(text.trimStart());
}

export function ConversationModeSelect({ mode, onChange, allowWrite = false }: {
  mode: ConversationMode;
  onChange: (mode: ConversationMode) => void;
  allowWrite?: boolean;
}) {
  const Icon = mode === "plan" ? Map : mode === "goal" ? Target : mode === "approved_write" ? Pencil : ShieldCheck;
  return (
    <span className="conv-option"><Icon size={15} aria-hidden /><ControlTooltip label={modeDescriptions[mode]} side="top"><select aria-label="Conversation mode" className="conv-select" value={mode}
      onChange={(event) => onChange(event.currentTarget.value as ConversationMode)}>
      <option value="read_only">Read-only</option>
      <option value="plan">Plan Mode</option>
      {allowWrite && <option value="approved_write">Approved write</option>}
      <option value="goal">Goal Mode</option>
    </select></ControlTooltip></span>
  );
}
