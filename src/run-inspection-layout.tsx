import { Activity, CircleDot } from "lucide-react";
import { Tabs } from "radix-ui";
import { useState, type ReactNode } from "react";
import { ContextActions, useContextActionFeedback, type MenuGroups } from "@/components/ui/context-actions";
import "@/run-inspection-layout.css";

type InspectionPage = { id: string; name: string; content: ReactNode };
type InspectionStatus = { label: string; tone?: "progress" | "warning" | "success" | "danger" };

export function RunInspectionLayout({
  as: Container = "section", title, label = "Run activity", runId, status, actions, menu = [], notice, pages, children,
}: {
  as?: "section" | "aside";
  title: string;
  label?: string;
  runId?: string;
  status?: InspectionStatus;
  actions?: ReactNode;
  menu?: MenuGroups;
  notice?: ReactNode;
  pages?: InspectionPage[];
  children?: ReactNode;
}) {
  const [page, setPage] = useState("");
  const currentPage = pages?.find(({ id }) => id === page) ?? pages?.[0];
  const { copy, feedback } = useContextActionFeedback();
  const menuGroups: MenuGroups = [...menu, [
    ...(runId ? [{ label: "Copy run ID", run: () => copy(runId) }] : []),
    ...(status ? [{ label: "Copy run summary", run: () => copy([title, runId, status.label].filter(Boolean).join("\n")) }] : []),
  ], ...(pages && currentPage ? [[{ label: "Go to", items: pages.map(({ id, name }) => ({
    label: name, disabled: id === currentPage.id, run: () => setPage(id),
  })) }]] : [])];

  return <Container className="work-inspection run-inspection" aria-label={label}>
    <header className="run-inspection-header">
      <span className="run-inspection-icon"><Activity size={17} aria-hidden="true" /></span>
      <div className="run-inspection-identity">
        <ContextActions label={runId ? `${title} ${runId}` : title} groups={menuGroups}>
          <div className="min-w-0">
            <h2>{title}</h2>
            {runId && <code>{runId}</code>}
          </div>
        </ContextActions>
      </div>
      {actions && <div className="run-inspection-actions">{actions}</div>}
      {status && <p className="run-inspection-status" data-tone={status.tone} role="status">
        <CircleDot size={13} aria-hidden="true" /> {status.label}
      </p>}
    </header>
    {feedback && <p className="px-3 py-2 text-xs" role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
    {notice && <div className="work-inspection-body run-inspection-body">{notice}</div>}
    {pages && currentPage ? <Tabs.Root value={currentPage.id} onValueChange={setPage} activationMode="manual">
      <Tabs.List className="work-inspection-tabs run-inspection-tabs" aria-label="Run details">
        {pages.map(({ id, name }) => <Tabs.Trigger key={id} value={id}>{name}</Tabs.Trigger>)}
      </Tabs.List>
      {pages.map(({ id, content }) => <Tabs.Content key={id} value={id} className="work-inspection-body run-inspection-body">
        {content}
      </Tabs.Content>)}
    </Tabs.Root> : <div className="work-inspection-body run-inspection-body">{children}</div>}
  </Container>;
}
