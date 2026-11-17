import { Info } from "lucide-react";
import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { ContextActions, type MenuGroups } from "@/components/ui/context-actions";
import type { ScenarioId, WorkspaceProps } from "@/workbench-contract";

export function IntegrationNotice({
  children,
  start,
  scenario,
  title = "Integration pending",
}: {
  children: ReactNode;
  start?: WorkspaceProps["startSimulation"];
  scenario?: ScenarioId;
  title?: string;
}) {
  return (
    <div className="settings-notice">
      <Info size={16} aria-hidden="true" />
      <div>
        <strong>{title}</strong>
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
  menu,
  children,
}: {
  id: string;
  label: string;
  description?: string;
  menu?: MenuGroups;
  children: ReactNode;
}) {
  return (
    <div className="settings-row" data-setting={id}>
      <div className="settings-row-label">
        <ContextActions label={label} groups={menu ?? []}><label htmlFor={id}>{label}</label></ContextActions>
        {description && <p id={`${id}-description`}>{description}</p>}
      </div>
      {children}
    </div>
  );
}
