import { useState } from "react";
import { Button } from "@/components/ui/button";
import { projectWorkModeSettings } from "@/project-agile-settings";
import { IntegrationNotice, SettingRow } from "@/settings-controls";
import type { WorkspaceProps } from "@/workbench-contract";

type Choice = readonly [value: string, label: string];

type Field = {
  id: string;
  label: string;
  description: string;
  initial: string;
} & (
  | { kind: "choice"; options: readonly Choice[] }
  | { kind: "number"; min: number; max: number }
  | { kind: "text"; placeholder?: string }
  | { kind: "toggle" }
);

export const policyFields = {
  capacity: [
    {
      id: "notifications", label: "Desktop notifications", kind: "toggle", initial: "true",
      description: "Notify when work needs input, waits for approval or finishes.",
    },
  ],
  execution: [
    {
      id: "worktree-root", label: "Managed worktree location", kind: "text", initial: "",
      description: "The app creates task worktrees here. Existing worktrees keep their paths.",
      placeholder: "Use the OmniAgent data directory",
    },
    {
      ...projectWorkModeSettings.branch, kind: "choice", initial: "isolated",
      options: [["isolated", "Branch per work item"], ["shared", "Shared branch per Task"]],
    },
    {
      ...projectWorkModeSettings.checklist, kind: "choice", initial: "track",
      options: [["track", "Track progress"], ["by-item", "Run by item"]],
    },
    {
      id: "writer-admission", label: "Repository writes", kind: "choice", initial: "approval",
      description: "Scope checked when an implementation job requests permission to write.",
      options: [["approval", "Approved task worktree"], ["read-only", "Read-only"]],
    },
    {
      id: "subagent-policy", label: "Subagent concurrency policy", kind: "choice", initial: "managed",
      description: "A strict total requires verified control over provider-native delegation.",
      options: [["managed", "Managed slots"], ["strict", "Strict total"]],
    },
    {
      id: "subagent-cap", label: "Subagent limit", kind: "number", initial: "3", min: 0, max: 32,
      description: "Maximum concurrent subagents admitted for one conversation.",
    },
    {
      id: "subagent-audit", label: "Subagent audit evidence", kind: "choice", initial: "available",
      description: "Require complete evidence to record each child’s input, provenance and response.",
      options: [["available", "Show available evidence"], ["complete", "Require complete evidence"]],
    },
    {
      id: "live-wait", label: "Live question timeout (minutes)", kind: "number", initial: "10", min: 1, max: 120,
      description: "Release the execution slot at a safe boundary and preserve the pending question.",
    },
  ],
  conversation: [
    {
      id: "queued-context", label: "Queued work after context changes", kind: "choice", initial: "review",
      description: "Applies after rewinding or clearing an active conversation.",
      options: [["review", "Pause for review"], ["recorded", "Use recorded context"], ["cancel", "Cancel pending jobs"]],
    },
    {
      id: "fork-checkout", label: "Default fork checkout", kind: "choice", initial: "ask",
      description: "The fork confirmation previews the base and effective write permissions.",
      options: [["ask", "Ask each time"], ["same", "Reuse this checkout"], ["worktree", "Create a linked worktree"]],
    },
    {
      id: "continuation", label: "When session continuation is unavailable", kind: "choice", initial: "ask",
      description: "Choose how conversation history is seeded after a provider or model change.",
      options: [["ask", "Ask each time"], ["here", "Continue here"], ["fork", "Fork with context"], ["fresh", "Start fresh"]],
    },
    {
      id: "suggestions", label: "Composer suggestions", kind: "choice", initial: "native",
      description: "Eligible requests can consume provider account usage.",
      options: [["off", "Off"], ["native", "Native when available"], ["omniagent", "Use OmniAgent when native is unavailable"]],
    },
    {
      id: "auto-compact", label: "Automatically compact context", kind: "toggle", initial: "true",
      description: "Create a summary from a recorded prefix before sending a turn over the threshold.",
    },
    {
      id: "compact-threshold", label: "Compaction threshold (%)", kind: "number", min: 10, max: 100, initial: "80",
      description: "Percentage of the available dialogue budget, accounting for required context and reserved output.",
    },
  ],
  automation: [
    {
      id: "implementation-release", label: "Implementation release", kind: "choice", initial: "human",
      description: "An effective release binds the accepted plan revision, project, checkout and applicable rule.",
      options: [["human", "Human decision"], ["rules", "Use configured rules"]],
    },
    {
      id: "settings-proposals", label: "Settings and template proposals", kind: "choice", initial: "review",
      description: "Automatic edits apply only when every proposed field matches this project’s user-managed allowlist.",
      options: [["review", "Review every proposal"], ["allowlist", "Automatically apply allowlisted fields"]],
    },
    {
      id: "allowed-settings-fields", label: "Allowlisted fields", kind: "text", initial: "",
      description: "Comma-separated keys eligible for automatic changes in this project.",
      placeholder: "Example: template.name, template.description",
    },
  ],
  attachments: [
    {
      id: "attachment-limit-count", label: "Files per turn", kind: "number", min: 0, max: 25, initial: "5",
      description: "Limit the number of attached files per turn.",
    },
    {
      id: "attachment-limit-file", label: "Bytes per file", kind: "number", min: 0, max: Number.MAX_SAFE_INTEGER, initial: "5242880",
      description: "Applies to native and staged attachments.",
    },
    {
      id: "attachment-limit-turn", label: "Total bytes per turn", kind: "number", min: 0, max: Number.MAX_SAFE_INTEGER, initial: "10485760",
      description: "Maximum combined size of native and staged attachments.",
    },
    {
      id: "attachment-limit-excerpt", label: "UTF-8 bytes per excerpt", kind: "number", min: 0, max: Number.MAX_SAFE_INTEGER, initial: "8192",
      description: "Text beyond the limit requires a smaller excerpt or another permitted route.",
    },
  ],
} satisfies Record<string, readonly Field[]>;

export function PolicyForm({
  fields,
  stored,
  save,
  simulate,
}: {
  fields: readonly Field[];
  stored: Record<string, string> | undefined;
  save?: (settings: Record<string, string>) => void;
  simulate?: WorkspaceProps["startSimulation"];
}) {
  const [draft, setDraft] = useState<Record<string, string>>(() =>
    stored ?? Object.fromEntries(fields.map(({ id, initial }) => [id, initial])));
  const [saved, setSaved] = useState(false);
  function edit(id: string, value: string) {
    setDraft((current) => ({ ...current, [id]: value }));
    setSaved(false);
  }

  return (
    <>
      {!save && (
        <IntegrationNotice start={simulate}>
          This page is ready for the project and scheduler policies API.
        </IntegrationNotice>
      )}
      <form onSubmit={(event) => {
        event.preventDefault();
        save?.(draft);
        setSaved(true);
      }}>
        <fieldset disabled={!save}>
          {fields.map((field) => {
            const input = <PolicyInput field={field}
              value={save ? draft[field.id] ?? field.initial : ""} change={(value) => edit(field.id, value)} />;
            return <SettingRow key={field.id} id={field.id} label={field.label} description={field.description}>
              {input}
            </SettingRow>;
          })}
          <div className="settings-save">
            <Button type="submit">Save</Button>
            {saved && <span role="status">Saved for this simulation.</span>}
          </div>
        </fieldset>
      </form>
    </>
  );
}

function PolicyInput({ field, value, change }: {
  field: Field; value: string; change: (value: string) => void;
}) {
  const description = `${field.id}-description`;
  if (field.kind === "toggle") {
    return (
      <input id={field.id} type="checkbox" aria-describedby={description} checked={value === "true"}
        onChange={(event) => change(String(event.target.checked))} />
    );
  }
  if (field.kind === "choice") {
    return (
      <select id={field.id} aria-describedby={description} value={value} onChange={(event) => change(event.target.value)}>
        {value === "" && <option value="">Unavailable</option>}
        {field.options.map(([key, label]) => <option key={key} value={key}>{label}</option>)}
      </select>
    );
  }
  return (
    <input id={field.id} aria-describedby={description} value={value}
      type={field.kind === "number" ? "number" : "text"}
      required={field.kind === "number"}
      min={field.kind === "number" ? field.min : undefined}
      max={field.kind === "number" ? field.max : undefined}
      placeholder={field.kind === "text" ? field.placeholder : undefined}
      onChange={(event) => change(event.target.value)} />
  );
}
