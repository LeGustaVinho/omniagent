import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Blocks, FolderGit2, Settings } from "lucide-react";
import { lazy, Suspense, useEffect, useState } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { Button } from "@/components/ui/button";
import { CheckoutSelector } from "@/checkout-selector";
import { fileLinkEvent, type FileLinkTarget } from "@/file-links";
import "@/launch-workspace.css";
import { useAppAppearance } from "@/settings-appearance";
import { useWorkspace } from "@/workspace-state";
import type { WorkspaceDestination } from "@/workbench-contract";

const Workbench = lazy(() => import("./workbench"));
const SettingsDialog = lazy(() => import("./settings-dialog"));
const noProjectAction = () => undefined;

export function App() {
  useAppAppearance();
  const { checkout, selectCheckout } = useWorkspace();
  const [choosing, setChoosing] = useState(false);
  const [restoring, setRestoring] = useState(true);
  const [error, setError] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [libraryTarget, setLibraryTarget] = useState<{ checkoutId: string; conversationId: string }>();
  const [workspaceTarget, setWorkspaceTarget] = useState<WorkspaceDestination & { checkoutId: string }>();
  const [fileTarget, setFileTarget] = useState<FileLinkTarget>();

  useEffect(() => {
    let disposed = false;
    void invoke<ProjectCheckout | null>("active_checkout").then(
      (selected) => { if (!disposed) selectCheckout(selected); },
      (failure: unknown) => { if (!disposed) setError(String(failure)); },
    ).finally(() => { if (!disposed) setRestoring(false); });
    return () => { disposed = true; };
  }, [selectCheckout]);

  useEffect(() => {
    if (restoring || !isTauri()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let draining = false;
    let available = false;
    const drain = async () => {
      available = true;
      if (draining || disposed) return;
      draining = true;
      while (available && !disposed) {
        available = false;
        try {
          const target = await invoke<FileLinkTarget | null>("take_pending_file_link");
          if (target && !disposed) {
            setLibraryTarget(undefined);
            setWorkspaceTarget(undefined);
            setFileTarget(target);
            selectCheckout(target.checkout);
            setError("");
          }
        } catch (error) {
          if (!disposed) setError(String(error));
        }
      }
      draining = false;
    };
    void listen(fileLinkEvent, () => void drain()).then((remove) => {
      if (disposed) { remove(); return; }
      unlisten = remove;
      return drain();
    }, () => drain());
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [restoring, selectCheckout]);

  async function activateCheckout(selected: ProjectCheckout, conversationId?: string, destination?: WorkspaceDestination) {
    await invoke("select_active_checkout", { checkoutId: selected.checkoutId });
    setFileTarget(undefined);
    setLibraryTarget(conversationId ? { checkoutId: selected.checkoutId, conversationId } : undefined);
    setWorkspaceTarget(destination ? { ...destination, checkoutId: selected.checkoutId } : undefined);
    selectCheckout(selected);
    setError("");
  }

  async function chooseRepository() {
    setChoosing(true);
    setError("");
    try {
      const directory = await invoke<string | null>("choose_repository");
      if (directory) {
        const selected = await invoke<ProjectCheckout>("register_project", { directory });
        await activateCheckout(selected);
      }
    } catch (error) {
      setError(String(error));
    } finally {
      setChoosing(false);
    }
  }

  const openRepository = (
    <Button className="shrink-0" variant={checkout ? "outline" : "default"} disabled={choosing || restoring} onClick={() => void chooseRepository()}>
      {choosing ? "Opening…" : "Open repository"}
    </Button>
  );

  return (
    <main className="work-shell flex h-dvh flex-col overflow-hidden">
      {!checkout && <header className="work-titlebar"><div className="work-wordmark">
        <span className="work-mark" aria-hidden="true"><Blocks size={15} /></span>OmniAgent
      </div><Button className="ml-auto" variant="ghost" onClick={() => setSettingsOpen(true)}><Settings size={16} />Settings</Button></header>}
      {error && !!checkout && <p role="alert" className="work-banner">{error}</p>}
      {restoring ? (
        <div className="launch-workspace"><p role="status" className="launch-status">Restoring workspace…</p></div>
      ) : checkout ? (
        <Suspense fallback={<p role="status" className="p-6 text-sm text-muted-foreground">Loading workspace…</p>}>
          <Workbench key={checkout.checkoutId} checkout={checkout} openRepository={openRepository} onSelectCheckout={activateCheckout}
            initialConversationId={libraryTarget?.checkoutId === checkout.checkoutId ? libraryTarget.conversationId : undefined}
            destination={workspaceTarget?.checkoutId === checkout.checkoutId ? workspaceTarget : undefined}
            fileTarget={fileTarget?.checkout.checkoutId === checkout.checkoutId ? fileTarget : undefined} />
        </Suspense>
      ) : (
        <section className="launch-workspace" aria-labelledby="launch-title">
          <div className="launch-card">
            <span className="work-start-icon" aria-hidden="true"><FolderGit2 size={24} /></span>
            <div>
              <h1 id="launch-title">Open a Git repository</h1>
              <p>Choose a repository folder or resume a registered worktree.</p>
            </div>
            <div className="launch-actions">
              {openRepository}
              <CheckoutSelector checkout={null} disabled={choosing} onSelect={(selected, destination) => activateCheckout(selected, undefined, destination)} />
            </div>
            {error && <p role="alert" className="launch-error">{error}</p>}
          </div>
        </section>
      )}
      {!checkout && settingsOpen && <Suspense fallback={null}><SettingsDialog checkout={null} simulation={null}
        startSimulation={noProjectAction} openWorkspace={noProjectAction} openActivity={noProjectAction} openChanges={noProjectAction}
        open onOpenChange={setSettingsOpen} /></Suspense>}
    </main>
  );
}
