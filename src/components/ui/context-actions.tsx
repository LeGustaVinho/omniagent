import { ChevronRight, Ellipsis } from "lucide-react";
import { ContextMenu, DropdownMenu } from "radix-ui";
import { useRef, useState, type ReactNode } from "react";
import "./context-actions.css";

export type MenuAction = {
  label: string;
  disabled?: boolean;
  destructive?: boolean;
} & ({ run: () => void } | { items: readonly MenuAction[] });

export type MenuGroups = readonly (readonly MenuAction[])[];

function ActionItems({ groups, variant }: { groups: MenuGroups; variant: "context" | "dropdown" }) {
  const { Item, Separator, Sub, SubTrigger, SubContent, Portal } = variant === "context" ? ContextMenu : DropdownMenu;
  function render(action: MenuAction) {
    if ("items" in action) {
      return <Sub key={action.label}>
        <SubTrigger className="context-action" disabled={action.disabled}>
          {action.label}<ChevronRight aria-hidden />
        </SubTrigger>
        <Portal><SubContent className="context-actions-menu" collisionPadding={8}>{action.items.map(render)}</SubContent></Portal>
      </Sub>;
    }
    return <Item key={action.label} className="context-action" disabled={action.disabled}
      data-destructive={action.destructive ? "" : undefined} onSelect={action.run}>{action.label}</Item>;
  }
  return groups.filter((group) => group.length).map((group, index) => (
    <div key={group[0]!.label} role="presentation">
      {index > 0 && <Separator className="context-actions-separator" />}
      {group.map(render)}
    </div>
  ));
}

export function ContextActions({ label, groups, children, className = "", title, stopPointerDown = false }: {
  label: string;
  groups: MenuGroups;
  children: ReactNode;
  className?: string;
  title?: string;
  stopPointerDown?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const keyboardTarget = useRef<HTMLElement>(null);
  if (!groups.some((group) => group.length)) return <>{children}</>;
  return <ContextMenu.Root>
    <ContextMenu.Trigger asChild>
      <div className={`context-actions-target ${className}`} title={title} onKeyDown={(event) => {
        if (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
        event.preventDefault();
        event.stopPropagation();
        keyboardTarget.current = event.target as HTMLElement;
        setOpen(true);
      }}>
        {children}
        <DropdownMenu.Root open={open} onOpenChange={setOpen}>
          <DropdownMenu.Trigger asChild>
            <button type="button" className="context-actions-more" aria-label={`Actions for ${label}`} title={`Actions for ${label}`}
              onClick={(event) => event.stopPropagation()}
              onPointerDown={stopPointerDown ? (event) => event.stopPropagation() : undefined}>
              <Ellipsis size={15} aria-hidden />
            </button>
          </DropdownMenu.Trigger>
          <DropdownMenu.Portal>
            <DropdownMenu.Content aria-label={label} className="context-actions-menu" align="end" sideOffset={4} collisionPadding={8}
              onCloseAutoFocus={(event) => {
                const target = keyboardTarget.current;
                keyboardTarget.current = null;
                if (!target?.isConnected || event.defaultPrevented) return;
                event.preventDefault();
                target.focus();
              }}>
              <ActionItems groups={groups} variant="dropdown" />
            </DropdownMenu.Content>
          </DropdownMenu.Portal>
        </DropdownMenu.Root>
      </div>
    </ContextMenu.Trigger>
    <ContextMenu.Portal>
      <ContextMenu.Content aria-label={label} className="context-actions-menu" collisionPadding={8}>
        <ActionItems groups={groups} variant="context" />
      </ContextMenu.Content>
    </ContextMenu.Portal>
  </ContextMenu.Root>;
}

export function PositionedContextActions({ label, groups, point, onClose, restoreFocus }: {
  label: string;
  groups: MenuGroups;
  point: { x: number; y: number };
  onClose: () => void;
  restoreFocus: () => void;
}) {
  return <DropdownMenu.Root open onOpenChange={(open) => { if (!open) onClose(); }}>
    <DropdownMenu.Trigger asChild>
      <button className="context-actions-anchor" type="button" aria-hidden tabIndex={-1} style={{ left: point.x, top: point.y }} />
    </DropdownMenu.Trigger>
    <DropdownMenu.Portal>
      <DropdownMenu.Content aria-label={label} aria-labelledby={undefined} className="context-actions-menu" align="start" collisionPadding={8}
        onCloseAutoFocus={(event) => { event.preventDefault(); restoreFocus(); }}>
        <ActionItems groups={groups} variant="dropdown" />
      </DropdownMenu.Content>
    </DropdownMenu.Portal>
  </DropdownMenu.Root>;
}
