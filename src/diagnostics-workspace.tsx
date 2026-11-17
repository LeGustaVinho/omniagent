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

export function DiagnosticsCard({ title, icon: Icon, children, action, heading: Heading = "h2" }: {
  title: string; icon: LucideIcon; children: ReactNode; action?: ReactNode; heading?: "h1" | "h2";
}) {
  const id = useId();
  return <section className="diagnostics-card" aria-labelledby={id}>
    <div className="diagnostics-card-heading">
      <span className="diagnostics-icon"><Icon size={17} aria-hidden="true" /></span>
      <Heading id={id} className="text-[15px] font-[650] [overflow-wrap:anywhere]">{title}</Heading>
    </div>
    <div className="diagnostics-card-body">{children}</div>
    {action && <div className="diagnostics-card-actions">{action}</div>}
  </section>;
}

export function DesktopDiagnostics({ openSystem, installation = <SystemInstallation />,
  executables = <DesktopCliPrerequisites />, openAttempt }: {
  openSystem?: () => void; installation?: ReactNode; executables?: ReactNode; openAttempt?: () => void;
}) {
  return <div className="diagnostics-cards">
    <DiagnosticsCard title="Current installation" icon={HardDrive}
      action={(openSystem || openAttempt) && <>
        {openSystem && <Button type="button" variant="outline" onClick={openSystem}>System settings</Button>}
        {openAttempt && <Button type="button" variant="outline" onClick={openAttempt}>Open run inspector</Button>}
      </>}>
      {installation}
    </DiagnosticsCard>
    <DiagnosticsCard title="Installed executables" icon={TerminalSquare}>
      {executables}
    </DiagnosticsCard>
  </div>;
}
