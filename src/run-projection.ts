import { Channel, invoke } from "@tauri-apps/api/core";
import { queryOptions, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { RunEvent } from "@/bindings/RunEvent";
import type { RunSnapshot } from "@/bindings/RunSnapshot";
import type { SubscriptionUpdate } from "@/bindings/SubscriptionUpdate";

const RECENT_LIMIT = 256;

export function fixtureProjection(checkoutId: string) {
  return queryOptions({
    queryKey: ["fixture-runs", checkoutId],
    queryFn: () => invoke<RunSnapshot>("run_snapshot", { checkoutId }),
    staleTime: Infinity,
  });
}

export function applyRunEvent(snapshot: RunSnapshot, checkout: ProjectCheckout, event: RunEvent): RunSnapshot {
  if (BigInt(event.seq) <= BigInt(snapshot.cursor)) return snapshot;

  let { runs } = snapshot;
  if (event.data.type === "status") {
    const { status } = event.data;
    if (runs.some((run) => run.id === event.runId)) {
      runs = runs.map((run) => run.id === event.runId ? { ...run, status, turnId: event.turnId } : run);
    } else {
      runs = [{
        id: event.runId,
        projectId: checkout.projectId,
        checkoutId: checkout.checkoutId,
        turnId: event.turnId,
        status,
      }, ...runs];
    }
  }
  return {
    cursor: event.seq,
    runs,
    recentEvents: [...snapshot.recentEvents, event].slice(-RECENT_LIMIT),
  };
}

export function useRunSubscription(checkout: ProjectCheckout) {
  const client = useQueryClient();
  const query = useQuery(fixtureProjection(checkout.checkoutId));
  const ready = query.isSuccess;
  const [sourceError, setSourceError] = useState("");
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    if (!ready) return;
    const { queryKey } = fixtureProjection(checkout.checkoutId);
    const current = client.getQueryData<RunSnapshot>(queryKey);
    if (!current) return;
    const { checkoutId } = checkout;
    let disposed = false;
    let cancelId: string | undefined;
    const release = (id: string) => {
      void invoke("unsubscribe_events", { id }).catch(() => undefined);
    };
    const channel = new Channel<SubscriptionUpdate>((message) => {
      if (disposed) return;
      if (message.type === "failed") {
        setSourceError(message.message);
        return;
      }
      client.setQueryData<RunSnapshot>(queryKey, (snapshot) =>
        snapshot && applyRunEvent(snapshot, checkout, message.event));
    });
    void invoke<string>("subscribe_events", { checkoutId, afterSeq: current.cursor, channel }).then(
      (id) => {
        if (disposed) release(id);
        else cancelId = id;
      },
      (error: unknown) => { if (!disposed) setSourceError(String(error)); },
    );
    return () => {
      disposed = true;
      if (cancelId) release(cancelId);
    };
  }, [attempt, checkout, client, ready]);

  return {
    error: sourceError || (query.error ? String(query.error) : ""),
    reconnect: () => {
      setSourceError("");
      setAttempt((current) => current + 1);
      if (query.isError) void query.refetch();
    },
  };
}
