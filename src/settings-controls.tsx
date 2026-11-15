import { Info } from "lucide-react";
import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";
import type { ScenarioId, WorkspaceProps } from "@/workbench-contract";

export function IntegrationNotice({
  children,
  start,
  scenario,
}: {
  children: ReactNode;
  start?: WorkspaceProps["startSimulation"];
  scenario?: ScenarioId;
}) {
  return (
    <div className="settings-notice">
      <Info size={16} aria-hidden="true" />
      <div>
        <strong>Integration pending</strong>
        <p>{children}</p>
        {!start && <p>Open a repository to try this workflow.</p>}
      </div>
      {start && <Button variant="outline" type="button" onClick={() => start(scenario)}>Simulate</Button>}
    </div>
  );
}

export function SettingRow({
  id,
  label,
  description,
  children,
}: {
  id: string;
  label: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <div className="settings-row" data-setting={id}>
      <div className="settings-row-label">
        <label htmlFor={id}>{label}</label>
        {description && <p id={`${id}-description`}>{description}</p>}
      </div>
      {children}
    </div>
  );
}
