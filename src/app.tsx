import { invoke } from "@tauri-apps/api/core";
import { Blocks, FolderGit2, Settings } from "lucide-react";
import { lazy, Suspense, useEffect, useState } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { Button } from "@/components/ui/button";
import { useAppAppearance } from "@/settings-appearance";
import { useWorkspace } from "@/workspace-state";

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

  useEffect(() => {
    let disposed = false;
    void invoke<ProjectCheckout | null>("active_checkout").then(
      (selected) => { if (!disposed) selectCheckout(selected); },
      (failure: unknown) => { if (!disposed) setError(String(failure)); },
    ).finally(() => { if (!disposed) setRestoring(false); });
    return () => { disposed = true; };
  }, [selectCheckout]);

  async function activateCheckout(selected: ProjectCheckout, conversationId?: string) {
    await invoke("select_active_checkout", { checkoutId: selected.checkoutId });
    setLibraryTarget(conversationId ? { checkoutId: selected.checkoutId, conversationId } : undefined);
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
      {error && <p role="alert" className="work-banner">{error}</p>}
      {restoring ? (
        <p role="status" className="p-6 text-sm text-muted-foreground">Restoring workspace…</p>
      ) : checkout ? (
        <Suspense fallback={<p role="status" className="p-6 text-sm text-muted-foreground">Loading workspace…</p>}>
          <Workbench key={checkout.checkoutId} checkout={checkout} openRepository={openRepository} onSelectCheckout={activateCheckout}
            initialConversationId={libraryTarget?.checkoutId === checkout.checkoutId ? libraryTarget.conversationId : undefined} />
        </Suspense>
      ) : (
        <section className="work-onboarding">
          <div className="work-start">
            <span className="work-start-icon" aria-hidden="true"><FolderGit2 size={24} /></span>
            <h1>Open a Git repository</h1>
            <p>Choose a repository or one of its linked worktrees.</p>
            {openRepository}
          </div>
        </section>
      )}
      {!checkout && settingsOpen && <Suspense fallback={null}><SettingsDialog checkout={null} simulation={null}
        startSimulation={noProjectAction} openWorkspace={noProjectAction} openActivity={noProjectAction} openChanges={noProjectAction}
        open onOpenChange={setSettingsOpen} /></Suspense>}
    </main>
  );
}
