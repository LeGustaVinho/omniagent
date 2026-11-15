import { Button } from "@/components/ui/button";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";

export function App() {
  const [status, setStatus] = useState<string>();

  async function checkRuntime() {
    try {
      setStatus(await invoke<string>("runtime_status"));
    } catch (error) {
      setStatus(String(error));
    }
  }

  return (
    <main className="grid min-h-dvh place-items-center p-6">
      <section className="w-full max-w-sm rounded-xl border border-border bg-card p-7">
        <h1 className="text-2xl font-semibold tracking-tight">OmniAgent</h1>
        <Button className="mt-6" onClick={() => void checkRuntime()}>
          Check desktop runtime
        </Button>
        <p className="mt-3 min-h-5 text-sm text-muted-foreground" role="status">{status}</p>
      </section>
    </main>
  );
}
