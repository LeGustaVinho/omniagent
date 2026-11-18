import { Brain, Check, ChevronDown, CircleDollarSign, Layers3 } from "lucide-react";
import { DropdownMenu } from "radix-ui";
import { useId, useRef, useState, type ReactNode } from "react";
import type { ModelOption } from "@/bindings/ModelOption";
import type { ModelRating } from "@/bindings/ModelRating";
import type { ModelSelection } from "@/bindings/ModelSelection";
import type { Provider } from "@/bindings/Provider";
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
  combinedCatalog?: boolean;
  previewDetails?: (option: ModelOption) => ReactNode;
}

type ModelOrder = "intelligence" | "price";
const orderKey = "omniagent:composer-model-order";
const providerNames: Record<Provider, string> = { codex: "Codex", claude: "Claude Code", antigravity: "Gemini · Antigravity" };

function preferredModelOrder(fallback: ModelOrder = "intelligence"): ModelOrder {
  try {
    const saved = localStorage.getItem(orderKey);
    return saved === "price" || saved === "intelligence" ? saved : fallback;
  } catch { return fallback; }
}

function compareModels(left: ModelOption, right: ModelOption, order: ModelOrder) {
  const leftValue = order === "price" ? left.rating?.apiReferenceCents : left.rating?.intelligenceTenths;
  const rightValue = order === "price" ? right.rating?.apiReferenceCents : right.rating?.intelligenceTenths;
  const difference = leftValue != null && rightValue != null
    ? (order === "price" ? leftValue - rightValue : rightValue - leftValue)
    : Number(leftValue == null) - Number(rightValue == null);
  return difference || left.name.localeCompare(right.name)
    || left.selection.provider.localeCompare(right.selection.provider) || left.selection.profileId.localeCompare(right.selection.profileId);
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

  const intelligence = rating.intelligenceTenths === null ? null : rating.intelligenceTenths / 10;
  return (
    <span className="mt-1 grid gap-1.5 text-muted-foreground">
      {intelligence !== null ? <ScoreMeter label="Intelligence" score={intelligence} displayed={intelligence.toFixed(1)} />
        : <span className="text-xs">Intelligence pending</span>}
      {rating.priceScore !== null && <ScoreMeter label="Price score" score={rating.priceScore} displayed={String(rating.priceScore)} price />}
      {rating.apiReferenceCents !== null ? <span className="text-xs">
        API reference {apiPrice.format(rating.apiReferenceCents / 100)} / 1M tokens
      </span> : <span className="text-xs">API reference price pending</span>}
    </span>
  );
}

export function ModelPicker({ label, options, selection, layout = "field", loading, disabled, combinedCatalog = false, previewDetails }: ModelPickerProps) {
  const labelId = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const [inspected, setInspected] = useState<string>();
  const [theme, setTheme] = useState<string>();
  const [order, setOrder] = useState<ModelOrder>(() => combinedCatalog ? preferredModelOrder() : "intelligence");
  const orderedOptions = combinedCatalog ? options.toSorted((left, right) => compareModels(left, right, order)) : options;
  const selected = selection.mode === "multiple" ? selection.selected : selection.selected ? [selection.selected] : [];
  const selectedKeys = new Set(selected.map(modelKey));
  const selectedOptions = selected.flatMap((choice) => {
    const option = options.find((entry) => modelKey(entry.selection) === modelKey(choice));
    return option ? [option] : [];
  });
  const preview = options.find((option) => modelKey(option.selection) === inspected) ?? selectedOptions[0] ?? orderedOptions.find(({ selectable }) => selectable);
  const reset = selection.mode === "single" ? selection.reset : undefined;
  let summary = selectedOptions[0]?.name ?? reset?.label ?? "Choose model";
  if (selection.mode === "multiple") summary = selectedOptions.length ? `${selectedOptions.length} selected` : "Choose models";
  if (loading && !selectedOptions.length && !options.length) summary = "Loading models…";

  function changeOrder(next: ModelOrder) {
    setOrder(next);
    try { localStorage.setItem(orderKey, next); } catch { /* Keep the preference for this menu. */ }
  }

  function optionLabel(option: ModelOption) {
    const source = providerNames[option.selection.provider];
    const sameName = combinedCatalog ? options.filter(({ name }) => name === option.name) : [];
    const sameProvider = sameName.filter(({ selection: entry }) => entry.provider === option.selection.provider).length > 1;
    const detail = combinedCatalog ? [source, sameProvider ? option.selection.profileId : undefined].filter(Boolean).join(" · ") : undefined;
    return { detail, label: sameName.length > 1 ? `${option.name} · ${detail}` : option.name };
  }

  const menu = (
    <DropdownMenu.Root onOpenChange={(open) => {
      setInspected(undefined);
      if (open) {
        if (combinedCatalog) setOrder((current) => preferredModelOrder(current));
        const scope = trigger.current?.closest("[data-app-theme]");
        setTheme(scope && scope !== document.documentElement ? scope.getAttribute("data-app-theme") ?? undefined : undefined);
      }
    }}>
      <DropdownMenu.Trigger ref={trigger} type="button" className="model-picker-trigger" data-layout={layout} data-catalog={combinedCatalog || undefined} disabled={disabled}
        aria-label={`${label} · ${summary}`} title={selectedOptions.map(({ name }) => name).join(", ") || undefined}>
        {combinedCatalog && <Layers3 size={15} aria-hidden />}
        <span>{summary}</span><ChevronDown size={13} aria-hidden />
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content className="model-picker-menu" data-catalog={combinedCatalog || undefined} data-app-theme={theme} aria-labelledby={labelId}
          side={layout === "inline" ? "top" : "bottom"} align="start" sideOffset={6} collisionPadding={10}>
          <header className={combinedCatalog ? "model-picker-catalog-header" : undefined}>
            <DropdownMenu.Label id={labelId} className="model-picker-heading">{label}</DropdownMenu.Label>
            {combinedCatalog && <DropdownMenu.RadioGroup value={order} className="model-picker-sort" aria-label="Sort models">
              <span className="model-picker-sort-caption">Sort by</span>
              <DropdownMenu.RadioItem className="model-picker-item" value="intelligence" aria-label="Intelligence, highest first" onSelect={(event) => {
                event.preventDefault(); changeOrder("intelligence");
              }}><Brain size={14} aria-hidden /> Intelligence</DropdownMenu.RadioItem>
              <DropdownMenu.RadioItem className="model-picker-item" value="price" aria-label="Price, lowest first" onSelect={(event) => {
                event.preventDefault(); changeOrder("price");
              }}><CircleDollarSign size={14} aria-hidden /> Price</DropdownMenu.RadioItem>
            </DropdownMenu.RadioGroup>}
          </header>
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
                {orderedOptions.map((option) => {
                  const key = modelKey(option.selection);
                  const { detail, label: accessibleName } = optionLabel(option);
                  const value = order === "price" ? option.rating?.apiReferenceCents : option.rating?.intelligenceTenths;
                  return <DropdownMenu.RadioItem key={key} value={key} {...itemProps(option, () => setInspected(key), accessibleName)}>
                    <ModelCheck /><ModelOptionLabel option={option} detail={detail} />
                    {combinedCatalog && value != null && <span className="model-picker-sort-value">{order === "price"
                      ? apiPrice.format(value / 100) : `${(value / 10).toFixed(1)}/100`}</span>}
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
          {!options.length && <p className="model-picker-empty">{loading ? "Checking available models…" : combinedCatalog ? "No models reported by the configured services." : "No models reported by this profile."}</p>}
          {preview && <div className="model-picker-rating" aria-label={`Ratings for ${preview.name}`}>
            {(combinedCatalog || previewDetails) && <div className="model-picker-rating-heading">
              {combinedCatalog && <strong className="model-picker-rating-name">{preview.name}</strong>}
              {previewDetails?.(preview)}
            </div>}
            <ModelRatingMeters rating={preview.rating} />
          </div>}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );

  return layout === "inline" ? menu : <div className="model-picker-field"><span className="model-picker-label">{label}</span>{menu}</div>;
}

function itemProps(option: ModelOption, inspect: () => void, label = option.name) {
  return {
    className: "model-picker-item",
    disabled: !option.selectable,
    "aria-label": option.selectable ? label : undefined,
    textValue: option.name,
    onFocus: inspect,
    onPointerMove: inspect,
  };
}

function ModelCheck() {
  return <span className="model-picker-check"><DropdownMenu.ItemIndicator><Check size={14} aria-hidden /></DropdownMenu.ItemIndicator></span>;
}

function ModelOptionLabel({ option, detail }: { option: ModelOption; detail?: string }) {
  return (
    <span className="min-w-0">
      <span className="block truncate" title={option.name}>{option.name}</span>
      {detail && <small>{detail}</small>}
      {!option.selectable && <small>{option.unavailableReason ?? "Unavailable in this profile"}</small>}
    </span>
  );
}
