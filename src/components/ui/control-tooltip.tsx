import { Tooltip } from "radix-ui";
import type { ReactElement, ReactNode } from "react";

export function ControlTooltip({ children, label, shortcut, side = "bottom" }: {
  children: ReactElement;
  label: ReactNode;
  shortcut?: string;
  side?: "top" | "right" | "bottom" | "left";
}) {
  return <Tooltip.Provider delayDuration={350}>
    <Tooltip.Root>
      <Tooltip.Trigger asChild>{children}</Tooltip.Trigger>
      <Tooltip.Portal>
        <Tooltip.Content className="work-tooltip" side={side} sideOffset={7} collisionPadding={8}>
          {label}{shortcut && <kbd>{shortcut}</kbd>}
        </Tooltip.Content>
      </Tooltip.Portal>
    </Tooltip.Root>
  </Tooltip.Provider>;
}
