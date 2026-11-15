import { invoke } from "@tauri-apps/api/core";
import { lazy, Suspense, useEffect, useState } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { Button } from "@/components/ui/button";
import { useWorkspace } from "@/workspace-state";

const Workbench = lazy(() => import("./workbench"));

export function App() {
  const { checkout, selectCheckout } = useWorkspace();
  const [choosing, setChoosing] = useState(false);
  const [restoring, setRestoring] = useState(true);
  const [error, setError] = useState("");

  useEffect(() => {
    let disposed = false;
    void invoke<ProjectCheckout | null>("active_checkout").then(
      (selected) => { if (!disposed && selected) selectCheckout(selected); },
      (failure: unknown) => { if (!disposed) setError(String(failure)); },
    ).finally(() => { if (!disposed) setRestoring(false); });
    return () => { disposed = true; };
  }, [selectCheckout]);

  async function chooseRepository() {
    setChoosing(true);
    setError("");
    try {
      const directory = await invoke<string | null>("choose_repository");
      if (directory) {
        const selected = await invoke<ProjectCheckout>("register_project", { directory });
        await invoke("select_active_checkout", { checkoutId: selected.checkoutId });
        selectCheckout(selected);
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
    <main className="flex h-dvh flex-col overflow-hidden">
      <header className="flex min-h-15 shrink-0 items-center gap-5 border-b border-border bg-card px-5">
        <div className="flex shrink-0 items-center gap-2.5 text-sm font-semibold tracking-tight">
          <span aria-hidden="true" className="size-2.5 rotate-45 rounded-xs bg-primary" />
          OmniAgent
        </div>
        {checkout && (
          <span title={checkout.checkoutRoot} className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">
            {checkout.checkoutRoot}
          </span>
        )}
        {checkout && openRepository}
      </header>
      {error && <p role="alert" className="border-b border-border px-5 py-3 text-sm text-red-300">{error}</p>}
      {restoring ? (
        <p role="status" className="p-6 text-sm text-muted-foreground">Restoring workspace…</p>
      ) : checkout ? (
        <Suspense fallback={<p role="status" className="p-6 text-sm text-muted-foreground">Loading workspace…</p>}>
          <Workbench key={checkout.checkoutId} checkout={checkout} />
        </Suspense>
      ) : (
        <section className="grid flex-1 place-items-center p-6">
          <div className="w-full max-w-md rounded-xl border border-border bg-card p-7">
            <h1 className="text-xl font-semibold tracking-tight">Open a Git repository</h1>
            <p className="mt-2 mb-5 text-sm text-muted-foreground">Choose a repository or one of its linked worktrees.</p>
            {openRepository}
          </div>
        </section>
      )}
    </main>
  );
}
