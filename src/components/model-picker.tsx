import { Check, ChevronDown } from "lucide-react";
import { DropdownMenu } from "radix-ui";
import { useId, useRef, useState, type ReactNode } from "react";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelRating } from "@/bindings/ModelRating";
import type { ModelSelection } from "@/bindings/ModelSelection";
import "./model-picker.css";

const apiPrice = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});

type PickerSelection =
  | {
    mode: "single";
    selected: ModelSelection | null;
    onChange: (value: ModelSelection) => void;
    reset?: { label: string; onSelect: () => void };
  }
  | { mode: "multiple"; selected: readonly ModelSelection[]; onChange: (value: ModelSelection[]) => void };

interface ModelPickerProps {
  label: string;
  options: readonly ModelOption[];
  selection: PickerSelection;
  layout?: "inline" | "field";
  loading?: boolean;
  disabled?: boolean;
  previewDetails?: (option: ModelOption) => ReactNode;
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

export function ModelPicker({ label, options, selection, layout = "field", loading, disabled, previewDetails }: ModelPickerProps) {
  const labelId = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const [inspected, setInspected] = useState<string>();
  const [theme, setTheme] = useState<string>();
  const selected = selection.mode === "multiple" ? selection.selected : selection.selected ? [selection.selected] : [];
  const selectedKeys = new Set(selected.map(modelKey));
  const selectedOptions = selected.flatMap((choice) => {
    const option = options.find((entry) => modelKey(entry.selection) === modelKey(choice));
    return option ? [option] : [];
  });
  const preview = options.find((option) => modelKey(option.selection) === inspected) ?? selectedOptions[0] ?? options.find(({ selectable }) => selectable);
  const reset = selection.mode === "single" ? selection.reset : undefined;
  let summary = selectedOptions[0]?.name ?? reset?.label ?? "Choose model";
  if (selection.mode === "multiple") summary = selectedOptions.length ? `${selectedOptions.length} selected` : "Choose models";
  if (loading) summary = "Loading models…";

  const menu = (
    <DropdownMenu.Root onOpenChange={(open) => {
      setInspected(undefined);
      if (open) {
        const scope = trigger.current?.closest("[data-app-theme]");
        setTheme(scope && scope !== document.documentElement ? scope.getAttribute("data-app-theme") ?? undefined : undefined);
      }
    }}>
      <DropdownMenu.Trigger ref={trigger} type="button" className="model-picker-trigger" data-layout={layout} disabled={disabled}
        aria-label={`${label} · ${summary}`} title={selectedOptions.map(({ name }) => name).join(", ") || undefined}>
        <span>{summary}</span><ChevronDown size={13} aria-hidden />
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content className="model-picker-menu" data-app-theme={theme} aria-labelledby={labelId}
          side={layout === "inline" ? "top" : "bottom"} align="start" sideOffset={6} collisionPadding={10}>
          <DropdownMenu.Label id={labelId} className="model-picker-heading">{label}</DropdownMenu.Label>
          <div className="model-picker-list">
            {selection.mode === "single" ? (
              <DropdownMenu.RadioGroup value={selection.selected ? modelKey(selection.selected) : ""} onValueChange={(key) => {
                if (!key) return reset?.onSelect();
                const chosen = options.find((option) => modelKey(option.selection) === key);
                if (chosen?.selectable) selection.onChange(chosen.selection);
              }}>
                {reset && <DropdownMenu.RadioItem className="model-picker-item" value="">
                  <ModelCheck />{reset.label}
                </DropdownMenu.RadioItem>}
                {options.map((option) => {
                  const key = modelKey(option.selection);
                  return <DropdownMenu.RadioItem key={key} value={key} {...itemProps(option, () => setInspected(key))}>
                    <ModelCheck /><ModelOptionLabel option={option} />
                  </DropdownMenu.RadioItem>;
                })}
              </DropdownMenu.RadioGroup>
            ) : options.map((option) => {
              const key = modelKey(option.selection);
              return <DropdownMenu.CheckboxItem key={key} checked={selectedKeys.has(key)}
                {...itemProps(option, () => setInspected(key))}
                onSelect={(event) => event.preventDefault()}
                onCheckedChange={(checked) => selection.onChange(checked
                  ? [...selection.selected, option.selection]
                  : selection.selected.filter((current) => modelKey(current) !== key))}>
                <ModelCheck /><ModelOptionLabel option={option} />
              </DropdownMenu.CheckboxItem>;
            })}
          </div>
          {!options.length && <p className="model-picker-empty">{loading ? "Checking available models…" : "No models reported by this profile."}</p>}
          {preview && <div className="model-picker-rating" aria-label={`Ratings for ${preview.name}`}><ModelRatingMeters rating={preview.rating} /></div>}
          {preview && previewDetails?.(preview)}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );

  return layout === "inline" ? menu : <div className="model-picker-field"><span className="model-picker-label">{label}</span>{menu}</div>;
}

function itemProps(option: ModelOption, inspect: () => void) {
  return {
    className: "model-picker-item",
    disabled: !option.selectable,
    "aria-label": option.selectable ? option.name : undefined,
    textValue: option.name,
    onFocus: inspect,
    onPointerMove: inspect,
  };
}

function ModelCheck() {
  return <span className="model-picker-check"><DropdownMenu.ItemIndicator><Check size={14} aria-hidden /></DropdownMenu.ItemIndicator></span>;
}

function ModelOptionLabel({ option }: { option: ModelOption }) {
  return (
    <span className="min-w-0">
      <span className="block truncate" title={option.name}>{option.name}</span>
      {!option.selectable && <small>{option.unavailableReason ?? "Unavailable in this profile"}</small>}
    </span>
  );
}
