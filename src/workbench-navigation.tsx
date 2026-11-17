import { ArrowRight, Search, X, type LucideIcon } from "lucide-react";
import { Dialog } from "radix-ui";
import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import { Button } from "@/components/ui/button";

export type Destination = {
  label: string;
  icon: LucideIcon;
  current?: boolean;
  select: () => void;
};

export function GoTo({ destinations, onClose }: {
  destinations: readonly Destination[];
  onClose: () => void;
}) {
  const listId = useId();
  const search = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const matches = destinations.filter(({ label }) => label.toLowerCase().includes(query.trim().toLowerCase()));
  const active = matches.length ? Math.min(index, matches.length - 1) : -1;
  const optionId = (offset: number) => listId + "-" + offset;

  useEffect(() => {
    if (active >= 0) document.getElementById(optionId(active))?.scrollIntoView?.({ block: "nearest" });
  }, [active, listId]);

  function choose(destination: Destination) {
    onClose();
    destination.select();
  }

  function navigate(event: KeyboardEvent<HTMLInputElement>) {
    if (event.nativeEvent.isComposing || !matches.length) return;
    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      event.preventDefault();
      const delta = event.key === "ArrowUp" ? -1 : 1;
      setIndex((active + delta + matches.length) % matches.length);
    } else if (event.key === "Enter") {
      event.preventDefault();
      const destination = matches[active];
      if (destination) choose(destination);
    }
  }

  return <Dialog.Root open onOpenChange={(open) => { if (!open) onClose(); }}>
    <Dialog.Portal>
      <Dialog.Overlay className="work-shade" />
      <Dialog.Content className="work-goto" onOpenAutoFocus={(event) => {
        event.preventDefault();
        search.current?.focus();
      }} onCloseAutoFocus={(event) => event.preventDefault()}>
        <header className="work-goto-heading">
          <Dialog.Title>Go to…</Dialog.Title>
          <Dialog.Close asChild><Button type="button" variant="ghost" className="ui-icon-button" aria-label="Close navigation"><X size={16} /></Button></Dialog.Close>
        </header>
        <Dialog.Description className="sr-only">Search workspace destinations and press Enter to open one.</Dialog.Description>
        <div className="work-search">
          <Search size={16} aria-hidden="true" />
          <input ref={search} role="combobox" aria-label="Search destinations" aria-autocomplete="list"
            aria-expanded="true" aria-controls={listId} aria-activedescendant={active >= 0 ? optionId(active) : undefined}
            placeholder="Search destinations…" value={query} onChange={(event) => {
              setQuery(event.currentTarget.value);
              setIndex(0);
            }} onKeyDown={navigate} />
        </div>
        <div id={listId} role="listbox" aria-label="Destinations" className="work-goto-results">
          {matches.map((destination, offset) => {
            const { label, icon: Icon, current } = destination;
            return <div key={label} id={optionId(offset)} role="option" aria-selected={active === offset}
              onPointerMove={() => setIndex(offset)} onMouseDown={(event) => event.preventDefault()}
              onClick={() => choose(destination)}>
              <Icon size={16} aria-hidden="true" /><span>{label}</span>
              {current && <small>Current</small>}
              {active === offset && <ArrowRight size={14} aria-hidden="true" />}
            </div>;
          })}
        </div>
        {!matches.length && <p className="work-goto-empty" role="status">No matching destinations.</p>}
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
