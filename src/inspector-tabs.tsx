import { DockviewReact, themeDark, type DockviewReadyEvent, type IDockviewHeaderActionsProps } from "dockview-react";
import { Maximize2, Minimize2, X } from "lucide-react";
import { createContext, useContext, useRef, type ReactNode } from "react";
import { Button } from "@/components/ui/button";

type InspectorSlots = {
  content: ReactNode;
  plan?: ReactNode;
  onExpand: () => void;
  onClose: () => void;
  expanded: boolean;
  title: string;
};

const SlotContext = createContext<InspectorSlots | null>(null);

function InspectorContent() {
  return useContext(SlotContext)?.content;
}

function PlanContent() {
  return useContext(SlotContext)?.plan;
}

function PanelActions({ activePanel }: IDockviewHeaderActionsProps) {
  const slots = useContext(SlotContext);
  if (!slots || !activePanel) return null;
  const ExpandIcon = slots.expanded ? Minimize2 : Maximize2;
  const label = slots.expanded ? "Restore side panel" : "Expand side panel";
  return <div className="flex gap-0.5 px-1">
    <Button variant="ghost" className="h-7 w-7 p-0" aria-label={label} title={label} onClick={slots.onExpand}><ExpandIcon size={14} /></Button>
    <Button variant="ghost" className="h-7 w-7 p-0" aria-label={"Close " + slots.title.toLowerCase()}
      title={"Close " + slots.title.toLowerCase()} onClick={slots.onClose}><X size={15} /></Button>
  </div>;
}

const components = { inspector: InspectorContent, plan: PlanContent };

export function InspectorTabs(slots: InspectorSlots) {
  const initialized = useRef<unknown>(null);

  function ready({ api }: DockviewReadyEvent) {
    if (initialized.current === api) return;
    initialized.current = api;
    api.addPanel({ id: "inspector", component: "inspector", title: slots.title });
    if (slots.plan) {
      api.addPanel({ id: "plan", component: "plan", title: "Plan", position: { referencePanel: "inspector", direction: "within" } });
      api.getPanel("inspector")?.api.setActive();
    }
    api.onDidRemovePanel(() => { if (api.panels.length === 0) slots.onClose(); });
  }

  return <SlotContext.Provider value={slots}>
    <div className="workspace-dock min-h-0 flex-1" aria-label={slots.title + " tabs"}>
      <DockviewReact
        components={components}
        onReady={ready}
        theme={themeDark}
        rightHeaderActionsComponent={PanelActions}
        disableFloatingGroups
        disableDnd
      />
    </div>
  </SlotContext.Provider>;
}
