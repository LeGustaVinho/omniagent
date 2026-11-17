import { FileDiff, GitBranch, History, Layers3, type LucideIcon } from "lucide-react";
import type { ReactNode } from "react";

type GitTab = "Changes" | "Worktrees" | "History" | "Saved attempts";
const tabIcons: Record<GitTab, LucideIcon> = { Changes: FileDiff, Worktrees: Layers3, History, "Saved attempts": FileDiff };

export function GitWorkspaceFrame<T extends GitTab>({ tabs, active, select, controls, children }: {
  tabs: readonly T[]; active: T; select: (tab: T) => void; controls?: ReactNode; children: ReactNode;
}) {
  return <section className="delivery-view" aria-label="Git workspace">
    <header className="delivery-toolbar">
      <GitBranch size={16} aria-hidden className="text-muted-foreground" />
      <h2>Git</h2>
      <nav aria-label="Git views" className="flex flex-wrap gap-1">
        {tabs.map((tab) => {
          const Icon: LucideIcon = tabIcons[tab];
          return <button key={tab} type="button" className="delivery-tab" aria-pressed={tab === active} onClick={() => select(tab)}>
            <Icon size={14} aria-hidden className="mr-1 inline-block" />{tab}
          </button>;
        })}
      </nav>
      {controls}
    </header>
    {children}
  </section>;
}

export function GitWorkspaceTable({ label, columns, children }: { label: string; columns: readonly string[]; children: ReactNode }) {
  return <div className="overflow-auto rounded-lg border border-border">
    <table className="delivery-table" aria-label={label}>
      <thead><tr>{columns.map((column) => <th key={column} scope="col">{column}</th>)}</tr></thead>
      <tbody>{children}</tbody>
    </table>
  </div>;
}
