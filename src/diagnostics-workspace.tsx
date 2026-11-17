import { Activity, FlaskConical, HardDrive, TerminalSquare, type LucideIcon } from "lucide-react";
import { useId, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { DesktopCliPrerequisites } from "@/desktop-cli-prerequisites";
import { SystemInstallation } from "@/system-installation";
import "./diagnostics-workspace.css";

const views = [
  { id: "desktop", label: "Desktop", icon: HardDrive },
  { id: "fixture", label: "Fixture runs", icon: FlaskConical },
] as const;
export type DiagnosticView = (typeof views)[number]["id"];

export function DiagnosticsWorkspace({ active, onSelect, desktop, fixtures }: {
  active: DiagnosticView;
  onSelect: (view: DiagnosticView) => void;
  desktop: ReactNode;
  fixtures?: ReactNode;
}) {
  const id = useId();
  return <section className="diagnostics-workspace" aria-label="Diagnostics workspace">
    <header className="diagnostics-heading">
      <span className="diagnostics-icon"><Activity size={20} aria-hidden="true" /></span>
      <h1>Diagnostics</h1>
    </header>
    <nav className="diagnostics-nav" aria-label="Diagnostic views">
      {views.map(({ id: view, label, icon: Icon }) => <Button key={view} type="button" variant="ghost"
        aria-controls={id + view} aria-current={active === view ? "page" : undefined}
        onClick={() => onSelect(view)}>
        <Icon size={16} aria-hidden="true" /> {label}
      </Button>)}
    </nav>
    <div className="diagnostics-view diagnostics-desktop" id={id + "desktop"} hidden={active !== "desktop"}>
      {desktop}
    </div>
    <div className="diagnostics-view diagnostics-fixtures" id={id + "fixture"} hidden={active !== "fixture"}>
      {fixtures}
    </div>
  </section>;
}

export function DiagnosticsCard({ title, icon: Icon, children, action }: {
  title: string; icon: LucideIcon; children: ReactNode; action?: ReactNode;
}) {
  const id = useId();
  return <section className="diagnostics-card" aria-labelledby={id}>
    <div className="diagnostics-card-heading">
      <span className="diagnostics-icon"><Icon size={17} aria-hidden="true" /></span>
      <h2 id={id}>{title}</h2>
    </div>
    <div className="diagnostics-card-body">{children}</div>
    {action && <div className="diagnostics-card-actions">{action}</div>}
  </section>;
}

export function DesktopDiagnostics({ openSystem }: { openSystem?: () => void }) {
  return <div className="diagnostics-cards">
    <DiagnosticsCard title="Current installation" icon={HardDrive}
      action={openSystem && <Button type="button" variant="outline" onClick={openSystem}>System settings</Button>}>
      <SystemInstallation />
    </DiagnosticsCard>
    <DiagnosticsCard title="Installed executables" icon={TerminalSquare}>
      <DesktopCliPrerequisites />
    </DiagnosticsCard>
  </div>;
}
