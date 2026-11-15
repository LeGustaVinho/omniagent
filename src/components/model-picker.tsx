import { Check, ChevronDown } from "lucide-react";
import { DropdownMenu } from "radix-ui";
import { useId, useState } from "react";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelRating } from "@/bindings/ModelRating";
import type { ModelSelection } from "@/bindings/ModelSelection";

const apiPrice = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});

type PickerSelection =
  | { mode: "single"; selected: ModelSelection | null; onChange: (value: ModelSelection) => void }
  | { mode: "multiple"; selected: readonly ModelSelection[]; onChange: (value: ModelSelection[]) => void };

interface ModelPickerProps {
  label: string;
  options: readonly ModelOption[];
  selection: PickerSelection;
}

function modelKey({ provider, profileId, modelId }: ModelSelection) {
  return JSON.stringify([provider, profileId, modelId]);
}

function ScoreMeter({ label, score, displayed, price = false }: {
  label: string;
  score: number;
  displayed: string;
  price?: boolean;
}) {
  return (
    <span className="block min-w-0">
      <span className="flex flex-wrap justify-between gap-x-2 text-xs">
        <span>{label}</span>
        <span className="shrink-0 tabular-nums">{displayed}/100</span>
      </span>
      <meter className={price ? "model-meter model-meter-price" : "model-meter"}
        min={0} max={100} value={score} aria-label={label} />
    </span>
  );
}

export function ModelRatingMeters({ rating }: { rating: ModelRating | null }) {
  if (!rating) {
    return <span className="text-xs text-muted-foreground">Unrated</span>;
  }

  const intelligence = rating.intelligenceTenths / 10;
  return (
    <span className="mt-1 grid gap-1.5 text-muted-foreground">
      <ScoreMeter label="Intelligence" score={intelligence} displayed={intelligence.toFixed(1)} />
      <ScoreMeter label="Price score" score={rating.priceScore} displayed={String(rating.priceScore)} price />
      <span className="text-xs">
        API reference {apiPrice.format(rating.apiReferenceCents / 100)} / 1M tokens
      </span>
    </span>
  );
}

export function ComposerModelPicker({ options, selected, onChange, loading }: {
  options: readonly ModelOption[];
  selected: ModelSelection | null;
  onChange: (value: ModelSelection) => void;
  loading?: boolean;
}) {
  const [inspected, setInspected] = useState<string>();
  const value = selected ? modelKey(selected) : "";
  const current = options.find((option) => modelKey(option.selection) === value);
  const preview = options.find((option) => modelKey(option.selection) === inspected) ?? current ?? options.find(({ selectable }) => selectable);

  return (
    <DropdownMenu.Root onOpenChange={() => setInspected(undefined)}>
      <DropdownMenu.Trigger className="conv-select conv-model-trigger" aria-label={current ? `Model · ${current.name}` : "Choose model"}>
        <span>{current?.name ?? (loading ? "Loading models…" : "Choose model")}</span><ChevronDown size={13} aria-hidden />
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content className="conv-model-menu" aria-label="Models" side="top" align="start" sideOffset={6} collisionPadding={10}>
          <DropdownMenu.Label className="conv-model-heading">Model</DropdownMenu.Label>
          <DropdownMenu.RadioGroup value={value} className="conv-model-list" onValueChange={(key) => {
            const chosen = options.find((option) => modelKey(option.selection) === key);
            if (chosen?.selectable) onChange(chosen.selection);
          }}>
            {options.map((option) => {
              const key = modelKey(option.selection);
              return <DropdownMenu.RadioItem className="conv-model-item" key={key} value={key} disabled={!option.selectable}
                aria-label={option.selectable ? option.name : undefined}
                textValue={option.name} onFocus={() => setInspected(key)} onPointerMove={() => setInspected(key)}>
                <span className="conv-model-item-check"><DropdownMenu.ItemIndicator><Check size={14} aria-hidden /></DropdownMenu.ItemIndicator></span>
                <span className="min-w-0">
                  <span className="block truncate" title={option.name}>{option.name}</span>
                  {!option.selectable && <small>{option.unavailableReason ?? "Unavailable in this profile"}</small>}
                </span>
              </DropdownMenu.RadioItem>;
            })}
          </DropdownMenu.RadioGroup>
          {!options.length && <p className="conv-model-empty">{loading ? "Checking available models…" : "No models reported by this profile."}</p>}
          {preview && <div className="conv-model-rating" aria-label={`Ratings for ${preview.name}`}><ModelRatingMeters rating={preview.rating} /></div>}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

export function ModelPicker({ label, options, selection }: ModelPickerProps) {
  const group = useId();
  const selected = selection.mode === "multiple" ? selection.selected : selection.selected ? [selection.selected] : [];
  const selectedKeys = new Set(selected.map(modelKey));
  const selectedOptions = options.filter((option) => selectedKeys.has(modelKey(option.selection)));

  function select(option: ModelOption) {
    if (!option.selectable) return;
    if (selection.mode === "single") {
      selection.onChange(option.selection);
      return;
    }
    const key = modelKey(option.selection);
    selection.onChange(selectedKeys.has(key)
      ? selection.selected.filter((current) => modelKey(current) !== key)
      : [...selection.selected, option.selection]);
  }

  return (
    <details className="w-full min-w-0 rounded-md border border-border bg-card">
      <summary className="cursor-pointer px-3 py-2 focus-visible:outline-ring">
        <span className="text-sm font-medium">{label}</span>
        {selectedOptions.length === 0 && <span className="ml-2 text-sm text-muted-foreground">Choose {selection.mode === "single" ? "a model" : "models"}</span>}
        {selectedOptions.length > 0 && (
          <span aria-label="Selected models" className="mt-2 grid gap-2">
            {selectedOptions.map((option) => (
              <span key={modelKey(option.selection)} className="min-w-0 break-words text-sm">
                {option.name}
                <ModelRatingMeters rating={option.rating} />
              </span>
            ))}
          </span>
        )}
      </summary>
      <fieldset className="max-h-80 min-w-0 overflow-y-auto border-t border-border p-2">
        <legend className="sr-only">{label}</legend>
        {options.length === 0 && <p className="p-2 text-sm text-muted-foreground">No models reported by this profile.</p>}
        {options.map((option) => {
          const key = modelKey(option.selection);
          return (
            <label key={key} className="flex min-w-0 gap-2 rounded-md px-2 py-3 hover:bg-muted has-disabled:cursor-not-allowed">
              <input
                className="mt-1 self-start accent-primary"
                name={group}
                type={selection.mode === "single" ? "radio" : "checkbox"}
                checked={selectedKeys.has(key)}
                disabled={!option.selectable}
                aria-label={option.name}
                onChange={() => select(option)}
              />
              <span className="min-w-0 flex-1 break-words">
                <span className="text-sm font-medium">{option.name}</span>
                {!option.selectable && (
                  <span className="ml-2 text-xs text-muted-foreground">{option.unavailableReason ?? "Unavailable in this profile"}</span>
                )}
                <ModelRatingMeters rating={option.rating} />
              </span>
            </label>
          );
        })}
      </fieldset>
    </details>
  );
}
