import { AtSign, Blocks, Terminal } from "lucide-react";
import { useLayoutEffect, useState, type RefObject } from "react";
import { createPortal } from "react-dom";
import "./composer-completion.css";

export type ComposerMenuOption = {
  id: string;
  kind: "conversation" | "command" | "customization";
  title: string;
  detail?: string;
  unavailable?: string;
  select: () => void;
};

type Position = { left: number; edge: number; width: number; height: number; above: boolean };

function caretPosition(textarea: HTMLTextAreaElement, cursor: number) {
  const style = getComputedStyle(textarea);
  const mirror = document.createElement("div");
  Object.assign(mirror.style, {
    position: "fixed", top: "0", left: "0", visibility: "hidden", pointerEvents: "none",
    whiteSpace: "pre-wrap", overflowWrap: "break-word", boxSizing: "border-box", width: `${textarea.clientWidth}px`,
  });
  for (const property of ["font", "letter-spacing", "line-height", "padding", "text-align", "direction", "tab-size"]) {
    mirror.style.setProperty(property, style.getPropertyValue(property));
  }
  const marker = document.createElement("span");
  marker.textContent = textarea.value.slice(cursor) || "\u200b";
  mirror.append(document.createTextNode(textarea.value.slice(0, cursor)), marker);
  document.body.append(mirror);
  const caret = marker.getClientRects()[0] ?? marker.getBoundingClientRect();
  mirror.remove();
  const bounds = textarea.getBoundingClientRect();
  return {
    left: bounds.left + textarea.clientLeft + caret.left - textarea.scrollLeft,
    top: Math.max(bounds.top, Math.min(bounds.bottom, bounds.top + textarea.clientTop + caret.top - textarea.scrollTop)),
    line: caret.height || Number.parseFloat(style.lineHeight) || 20,
  };
}

const icons = { conversation: AtSign, command: Terminal, customization: Blocks };

export function ComposerCompletionMenu({ field, cursor, active, id, label, options, status, error, onHighlight, onDismiss }: {
  field: RefObject<HTMLTextAreaElement | null>;
  cursor: number;
  active: number;
  id: string;
  label: string;
  options: readonly ComposerMenuOption[];
  status?: string;
  error?: string;
  onHighlight: (index: number) => void;
  onDismiss: () => void;
}) {
  const [position, setPosition] = useState<Position>();
  useLayoutEffect(() => {
    const element = field.current;
    if (!element) return;
    const update = () => {
      const caret = caretPosition(element, cursor);
      const width = Math.min(398, window.innerWidth - 20);
      const below = window.innerHeight - caret.top - caret.line - 16;
      const above = below < 220 && caret.top - 16 > below;
      setPosition({ left: Math.max(10, Math.min(caret.left, window.innerWidth - width - 10)),
        edge: above ? window.innerHeight - caret.top + 6 : caret.top + caret.line + 6,
        width, height: Math.max(44, Math.min(292, above ? caret.top - 16 : below)), above });
    };
    update();
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    const observer = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(update);
    observer?.observe(element);
    return () => {
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
      observer?.disconnect();
    };
  }, [cursor, field, options.length]);

  useLayoutEffect(() => {
    if (active >= 0) document.getElementById(`${id}-${active}`)?.scrollIntoView?.({ block: "nearest" });
  }, [active, id]);

  if (!position) return null;
  return createPortal(<div className="composer-completion-menu" style={{
    left: position.left, width: position.width, maxHeight: position.height,
    ...(position.above ? { bottom: position.edge } : { top: position.edge }),
  }} onMouseDown={(event) => event.preventDefault()}>
    <div className="composer-completion-heading"><strong>{label}</strong>
      <button type="button" className="composer-completion-close" tabIndex={-1} aria-label={`Close ${label.toLowerCase()}`} onClick={onDismiss}>Esc</button>
    </div>
    {error && <p className="composer-completion-status" role="alert">{error}</p>}
    {status && <p className="composer-completion-status" role="status">{status}</p>}
    <div id={id} className="composer-completion-options" role="listbox" aria-label={label}>
      {options.map((option, index) => {
        const Icon = icons[option.kind];
        return <div key={option.id} id={`${id}-${index}`} role="option" aria-selected={index === active}
          aria-disabled={Boolean(option.unavailable)} onMouseMove={() => { if (!option.unavailable) onHighlight(index); }}
          onClick={() => { if (!option.unavailable) option.select(); }}>
          <Icon size={16} aria-hidden />
          <div className="min-w-0"><strong>{option.title}</strong>{(option.unavailable || option.detail) && <small>{option.unavailable ?? option.detail}</small>}</div>
        </div>;
      })}
    </div>
  </div>, document.body);
}
