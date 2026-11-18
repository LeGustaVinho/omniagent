import { X } from "lucide-react";
import { Dialog } from "radix-ui";
import type { ReactNode } from "react";
import { Button } from "@/components/ui/button";
import "./workflow-builder.css";

export function WorkflowBuilderDialog({ title, description, trigger, wide, open, onOpenChange, onOpenAutoFocus, onCloseAutoFocus,
  children, footer, closeDisabled }: {
  title: string; description: string; trigger?: ReactNode; wide?: boolean;
  open?: boolean; onOpenChange?: (open: boolean) => void; closeDisabled?: boolean;
  onOpenAutoFocus?: (event: Event) => void; onCloseAutoFocus?: (event: Event) => void;
  children?: ReactNode; footer?: ReactNode;
}) {
  return <Dialog.Root open={open} onOpenChange={onOpenChange}>
    {trigger && <Dialog.Trigger asChild>{trigger}</Dialog.Trigger>}
    <Dialog.Portal>
      <Dialog.Overlay className="workflow-builder-shade" />
      <Dialog.Content className="workflow-builder-dialog" data-wide={wide || undefined}
        onOpenAutoFocus={onOpenAutoFocus} onCloseAutoFocus={onCloseAutoFocus}>
        <header>
          <div><Dialog.Title>{title}</Dialog.Title><Dialog.Description>{description}</Dialog.Description></div>
          <Dialog.Close asChild><Button type="button" variant="ghost" className="ui-icon-button" disabled={closeDisabled}
            aria-label={`Close ${title.toLowerCase()}`}>
            <X size={17} aria-hidden />
          </Button></Dialog.Close>
        </header>
        {children && <div className="workflow-builder-dialog-body pipeline-stack">{children}</div>}
        {footer && <footer className="workflow-builder-dialog-footer">{footer}</footer>}
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
