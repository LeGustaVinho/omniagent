import { createContext, useCallback, useContext, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { WorkflowBuilderDialog } from "@/workflow-builder-dialog";

type DraftNavigationAction = (discarded: boolean) => void;

const DraftNavigation = createContext<{
  navigate: (action: DraftNavigationAction) => void;
  register: () => () => void;
} | undefined>(undefined);

const runImmediately = (action: DraftNavigationAction) => action(false);

export function useWorkflowDraftNavigation() {
  return useContext(DraftNavigation)?.navigate ?? runImmediately;
}

export function useUnsavedWorkflowDraft(dirty: boolean) {
  const navigation = useContext(DraftNavigation);
  useLayoutEffect(() => dirty ? navigation?.register() : undefined, [dirty, navigation]);
}

export function WorkflowDraftNavigation({ children }: { children: ReactNode }) {
  const dirtyEditors = useRef(0);
  const origin = useRef<HTMLElement | null>(null);
  const keepEditing = useRef<HTMLButtonElement>(null);
  const [pending, setPending] = useState<DraftNavigationAction>();
  const navigate = useCallback((action: DraftNavigationAction) => {
    if (!dirtyEditors.current) return action(false);
    origin.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setPending(() => action);
  }, []);
  const navigation = useMemo(() => ({
    navigate,
    register() {
      dirtyEditors.current++;
      return () => { dirtyEditors.current--; };
    },
  }), [navigate]);
  return <DraftNavigation.Provider value={navigation}>
    {children}
    <WorkflowBuilderDialog title="Unsaved changes" description="Save your changes before leaving, or discard them to continue."
      open={!!pending} onOpenChange={(open) => { if (!open) setPending(undefined); }}
      onOpenAutoFocus={(event) => { event.preventDefault(); keepEditing.current?.focus(); }}
      onCloseAutoFocus={(event) => {
        event.preventDefault();
        if (origin.current?.isConnected && !origin.current.closest("[hidden]")) origin.current.focus({ preventScroll: true });
      }}
      footer={<>
        <Button ref={keepEditing} type="button" variant="outline" onClick={() => setPending(undefined)}>Keep editing</Button>
        <Button type="button" className="workflow-delete-button" onClick={() => {
          setPending(undefined);
          pending?.(true);
        }}>Discard changes and continue</Button>
      </>} />
  </DraftNavigation.Provider>;
}
