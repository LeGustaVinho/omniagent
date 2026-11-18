import { X } from "lucide-react";
import { Dialog } from "radix-ui";
import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";
import "./workflow-builder.css";

export function WorkflowBuilderDialog({ title, description, trigger, wide, open, onOpenChange, onCloseAutoFocus, children }: {
  title: string; description: string; trigger: ReactNode; wide?: boolean;
  open?: boolean; onOpenChange?: (open: boolean) => void; onCloseAutoFocus?: (event: Event) => void; children: ReactNode;
}) {
  return <Dialog.Root open={open} onOpenChange={onOpenChange}>
    <Dialog.Trigger asChild>{trigger}</Dialog.Trigger>
    <Dialog.Portal>
      <Dialog.Overlay className="workflow-builder-shade" />
      <Dialog.Content className="workflow-builder-dialog" data-wide={wide || undefined} onCloseAutoFocus={onCloseAutoFocus}>
        <header>
          <div><Dialog.Title>{title}</Dialog.Title><Dialog.Description>{description}</Dialog.Description></div>
          <Dialog.Close asChild><Button type="button" variant="ghost" className="ui-icon-button" aria-label={`Close ${title.toLowerCase()}`}>
            <X size={17} aria-hidden />
          </Button></Dialog.Close>
        </header>
        <div className="workflow-builder-dialog-body pipeline-stack">{children}</div>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
