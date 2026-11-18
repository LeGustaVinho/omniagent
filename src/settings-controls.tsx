import { Info } from "lucide-react";
import type { ReactNode } from "react";
import { ContextActions, type MenuGroups } from "@/components/ui/context-actions";

export function IntegrationNotice({
  children,
  title = "Integration pending",
}: {
  children: ReactNode;
  title?: string;
}) {
  return (
    <div className="settings-notice">
      <Info size={16} aria-hidden="true" />
      <div>
        <strong>{title}</strong>
        <p>{children}</p>
      </div>
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
