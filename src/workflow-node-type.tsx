import { ControlButton } from "@xyflow/react";
import { Bot, CircleHelp, Cog, Info, Layers3, Repeat2, Waypoints, Workflow } from "lucide-react";
import { Popover } from "radix-ui";
import type { WorkflowNodeAppearance } from "@/workflow-node-type-model";
import "./workflow-node-type.css";

const nodeTypes = {
  ai: { label: "AI", icon: Bot },
  "no-ai": { label: "No AI", icon: Cog },
  function: { label: "Function", icon: Waypoints },
  prefab: { label: "Prefab", icon: Layers3 },
  loop: { label: "Loop", icon: Repeat2 },
  reusable: { label: "Reusable node", icon: Workflow },
  unknown: { label: "Unknown type", icon: CircleHelp },
} satisfies Record<WorkflowNodeAppearance["type"], { label: string; icon: typeof Bot }>;

const legendTypes = ["ai", "no-ai", "function", "prefab"] as const;

function primaryLabel({ type, usesAI }: WorkflowNodeAppearance) {
  return type === "loop" && usesAI === false ? "No AI" : nodeTypes[type].label;
}

function hasAIBadge({ type, usesAI }: WorkflowNodeAppearance) {
  return type !== "ai" && usesAI === true;
}

export function workflowNodeTypeLabel(appearance: WorkflowNodeAppearance) {
  return primaryLabel(appearance) + (hasAIBadge(appearance) ? " · AI" : "");
}

export function WorkflowNodeBadge({ appearance }: { appearance: WorkflowNodeAppearance }) {
  const { icon: Icon } = nodeTypes[appearance.type];
  return <span className="workflow-node-type" data-node-type={appearance.type}>
    <span className="workflow-node-type-label"><Icon size={14} aria-hidden />{primaryLabel(appearance)}</span>
    {hasAIBadge(appearance) && <span className="workflow-node-ai-badge">AI</span>}
  </span>;
}

export function WorkflowNodeLegend() {
  return <Popover.Root>
    <Popover.Trigger asChild>
      <ControlButton className="workflow-node-legend-trigger" aria-label="Node types" title="Node types"><Info aria-hidden /></ControlButton>
    </Popover.Trigger>
    <Popover.Portal>
      <Popover.Content className="workflow-node-legend" aria-label="Node types" side="top" align="start" sideOffset={8} collisionPadding={10}>
        <p>Node types</p>
        <ul>{legendTypes.map((type) => <li key={type}><WorkflowNodeBadge appearance={{ type }} /></li>)}</ul>
        <small>The AI badge shows that a reusable node or Loop can call a model.</small>
      </Popover.Content>
    </Popover.Portal>
  </Popover.Root>;
}
