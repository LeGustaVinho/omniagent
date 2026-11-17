import { AgileTableFrame } from "@/agile-layout";
import { ChevronDown, ChevronRight, GitBranch, GitCommitHorizontal } from "lucide-react";
import { Fragment, useState } from "react";
import { DeliveryStatus } from "@/tasks-git-ui";
import { modelName, workCode, workKind, type ChecklistItem, type DeliveryTask } from "@/tasks-demo-data";

export type WorkEvidence = { branch: string; commit: string | null };

export function AgileTable({ items, matches, selected, onSelect, onCheck, evidence, openGit }: {
  items: DeliveryTask[];
  matches: Set<string>;
  selected?: string;
  onSelect: (id: string) => void;
  onCheck: (workId: string, itemId: string, checked: boolean) => void;
  evidence: (work: DeliveryTask) => WorkEvidence | undefined;
  openGit: (target: "working" | "saved") => void;
}) {
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const children = new Map<string | undefined, DeliveryTask[]>();
  for (const work of items) children.set(work.parentId, [...children.get(work.parentId) ?? [], work]);

  function containsMatch(work: DeliveryTask): boolean {
    return matches.has(work.id) || (children.get(work.id)?.some(containsMatch) ?? false);
  }

  function toggle(id: string) {
    setExpanded((prior) => {
      const next = new Set(prior);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function itemRow(work: DeliveryTask, item: ChecklistItem, depth: number) {
    return <tr className="agile-checklist-row" key={item.id}>
      <td style={{ paddingInlineStart: `${1 + depth * 1.25}rem` }}>
        <label className="agile-check"><input type="checkbox" checked={item.checked}
          onChange={(event) => onCheck(work.id, item.id, event.currentTarget.checked)} /><span>{item.title}</span></label>
      </td>
      <td className="text-muted-foreground">{item.checked ? "Checked" : "Open"}</td>
      <td colSpan={4} className="text-muted-foreground">Checklist</td>
    </tr>;
  }

  function rows(list: DeliveryTask[], depth: number) {
    return list.filter(containsMatch).map((work) => {
      const nested = children.get(work.id)?.filter(containsMatch) ?? [];
      const checklist = matches.has(work.id) ? work.checklist ?? [] : [];
      const open = expanded.has(work.id);
      const refs = evidence(work);
      return <Fragment key={work.id}>
        <tr data-selected={work.id === selected}>
          <td style={{ paddingInlineStart: `${0.7 + depth * 1.25}rem` }}>
            <div className="agile-work-cell">
              {nested.length || checklist.length
                ? <button type="button" className="agile-disclosure" aria-label={`Expand ${work.title}`}
                    aria-expanded={open} onClick={() => toggle(work.id)}>{open ? <ChevronDown size={15} /> : <ChevronRight size={15} />}</button>
                : <span className="agile-disclosure" aria-hidden="true" />}
              <button type="button" aria-pressed={work.id === selected} className="agile-work-name" onClick={() => onSelect(work.id)}>
                <small>{workCode(work)} · {workKind(work)}</small><strong>{work.title}</strong>
              </button>
            </div>
          </td>
          <td><DeliveryStatus>{work.state}</DeliveryStatus></td>
          <td>{workKind(work) === "Epic" ? "—" : work.priority}</td>
          <td>{workKind(work) === "Epic" ? "—" : modelName(work.assignment)}</td>
          <td>{refs ? <button type="button" className="agile-ref" onClick={() => openGit("working")}><GitBranch size={13} />{refs.branch}</button> : "—"}</td>
          <td>{refs?.commit ? <button type="button" className="agile-ref" onClick={() => openGit("saved")}><GitCommitHorizontal size={13} />{refs.commit}</button> : "—"}</td>
        </tr>
        {open && <>{checklist.map((item) => itemRow(work, item, depth + 1))}{rows(nested, depth + 1)}</>}
      </Fragment>;
    });
  }

  return <AgileTableFrame columns={["Work", "State", "Priority", "Assignment", "Branch", "Commit"]}>
    {rows(children.get(undefined) ?? [], 0)}
  </AgileTableFrame>;
}
