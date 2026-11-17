import type { FormEventHandler, ReactNode } from "react";
import type { ModelRating } from "@/bindings/ModelRating";
import { ModelRatingMeters } from "@/components/model-picker";

export function PipelineComparisonSetup({ request, candidates, onSubmit }: {
  request: ReactNode;
  candidates: ReactNode;
  onSubmit: FormEventHandler<HTMLFormElement>;
}) {
  return <form className="pipeline-two-columns" onSubmit={onSubmit}>
    <section className="pipeline-box pipeline-form pipeline-stack">
      <h3>Request and context</h3>
      {request}
    </section>
    <section className="pipeline-box pipeline-stack">
      <h3>Candidates</h3>
      {candidates}
    </section>
  </form>;
}

export function PipelineComparisonCard({ name, state, rating, children }: {
  name: string; state: string; rating: ModelRating | null; children: ReactNode;
}) {
  return <li className="pipeline-box pipeline-stack">
    <header className="pipeline-card-heading">
      <h3 className="break-words">{name}</h3>
      <span role="status" className="pipeline-status" data-status={state}>{state.replaceAll("_", " ")}</span>
    </header>
    <ModelRatingMeters rating={rating} />
    {children}
  </li>;
}

export function PipelineComparisonChoice({ checked, onChange }: { checked: boolean; onChange: (checked: boolean) => void }) {
  return <label className="pipeline-checkline">
    <input type="checkbox" checked={checked} onChange={(event) => onChange(event.currentTarget.checked)} />
    Use this result
  </label>;
}
