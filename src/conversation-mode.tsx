export type ConversationMode = "read_only" | "plan" | "goal";

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

export function ConversationModeSelect({ mode, onChange }: {
  mode: ConversationMode;
  onChange: (mode: ConversationMode) => void;
}) {
  return (
    <select aria-label="Conversation mode" className="conv-select" value={mode}
      onChange={(event) => onChange(event.currentTarget.value as ConversationMode)}>
      <option value="read_only">Read-only</option>
      <option value="plan">Plan Mode</option>
      <option value="goal">Goal Mode</option>
    </select>
  );
}
