export type EffortSet = {
  efforts: readonly string[];
  defaultEffort?: string | null;
};

function effortLabel(value: string) {
  if (value === "xhigh") return "Extra high";
  return value.charAt(0).toUpperCase() + value.slice(1);
}

export function EffortSelect({ capabilities, value, onChange, label = "Effort", loading, className }: {
  capabilities?: EffortSet;
  value: string;
  onChange: (value: string) => void;
  label?: string;
  loading?: boolean;
  className?: string;
}) {
  if (!loading && !capabilities?.efforts.length) return null;
  const efforts = [...new Set(capabilities?.efforts)];
  const defaultEffort = capabilities?.defaultEffort;
  const defaultLabel = defaultEffort ? `Default (${effortLabel(defaultEffort)})` : "Default effort";
  return (
    <select className={className} aria-label={label} value={efforts.includes(value) ? value : ""}
      disabled={loading || !efforts.length} onChange={(event) => onChange(event.currentTarget.value)}>
      <option value="">{loading ? "Loading effort…" : defaultLabel}</option>
      {efforts.map((effort) => <option key={effort} value={effort}>{effortLabel(effort)}</option>)}
    </select>
  );
}
