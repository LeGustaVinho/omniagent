import { Dialog } from "radix-ui";
import { createContext, useCallback, useContext, useLayoutEffect, useRef, useState, type ComponentProps, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";
import { ResizablePanel } from "@/panel-resize";

export const WorkspaceVisibility = createContext(true);
export const WorkspaceDrawersSuspended = createContext(false);

export function useWorkspaceSize(ref: RefObject<HTMLElement | null>) {
  const [size, setSize] = useState(() => ({ width: window.innerWidth, height: window.innerHeight }));
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const measure = () => {
      const width = element.clientWidth;
      const height = element.clientHeight;
      if (width > 0 && height > 0) {
        setSize((previous) => previous.width === width && previous.height === height ? previous : { width, height });
      }
    };
    measure();
    const observer = typeof ResizeObserver !== "undefined" ? new ResizeObserver(measure) : undefined;
    observer?.observe(element);
    window.addEventListener("resize", measure);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [ref]);
  return size;
}

export function WorkbenchPanel({ drawer, open, label, drawerClass, onClose, children, ...panel }: {
  drawer: boolean;
  open: boolean;
  label: string;
  drawerClass: string;
  onClose: () => void;
  children: ReactNode;
} & Omit<ComponentProps<typeof ResizablePanel>, "hidden" | "children">) {
  const active = useContext(WorkspaceVisibility);
  const suspended = useContext(WorkspaceDrawersSuspended);
  const visible = active && open && !(drawer && suspended);
  const [host] = useState(() => {
    const element = document.createElement("div");
    element.className = "work-panel-body";
    return element;
  });
  const focusedControl = useRef<HTMLElement | null>(null);
  // The portal container stays the same when a panel moves into or out of a drawer.
  const attach = useCallback((slot: HTMLDivElement | null) => {
    if (!slot) {
      const focused = document.activeElement;
      if (focused instanceof HTMLElement && host.contains(focused)) focusedControl.current = focused;
      return;
    }
    slot.appendChild(host);
    const focused = focusedControl.current;
    focusedControl.current = null;
    if (focused && !slot.closest("[hidden]")) focused.focus({ preventScroll: true });
  }, [host]);
  const body = <div className="work-panel-slot" ref={attach} />;

  return <>
    {drawer && visible ? <Dialog.Root open onOpenChange={(next) => { if (!next) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="work-shade" />
        <Dialog.Content aria-describedby={undefined} asChild onCloseAutoFocus={(event) => event.preventDefault()}>
          <ResizablePanel {...panel} as="div" className={panel.className + " " + drawerClass}>
            <Dialog.Title className="sr-only">{label}</Dialog.Title>
            {body}
          </ResizablePanel>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root> : <ResizablePanel {...panel} hidden={!visible}>{body}</ResizablePanel>}
    {createPortal(children, host)}
  </>;
}
