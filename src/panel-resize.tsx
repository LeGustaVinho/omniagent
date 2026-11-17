import { useId, useLayoutEffect, useRef, useState, type ComponentPropsWithRef, type KeyboardEvent, type PointerEvent } from "react";
import "./panel-resize.css";

type ResizeEdge = "left" | "right" | "top" | "bottom";

type ResizablePanelProps = ComponentPropsWithRef<"div"> & {
  as?: "aside" | "section" | "div";
  resizeKey: string;
  resizeLabel: string;
  edge: ResizeEdge | readonly ResizeEdge[];
  centered?: boolean;
};

export function ResizablePanel({ as: Panel = "div", id, resizeKey, resizeLabel, edge, centered, className, children, ...props }: ResizablePanelProps) {
  const fallbackId = useId();
  const panelId = id ?? fallbackId;
  const edges = typeof edge === "string" ? [edge] : edge;
  return <Panel {...props} id={panelId} className={className ? `resizable-panel ${className}` : "resizable-panel"}>
    {children}
    {edges.map((side) => <ResizeHandle key={side} edge={side} panelId={panelId} resizeKey={resizeKey} label={resizeLabel} centered={centered} />)}
  </Panel>;
}

type Drag = { pointer: number; start: number; size: number; previous: string };

function ResizeHandle({ edge, panelId, resizeKey, label, centered }: {
  edge: ResizeEdge; panelId: string; resizeKey: string; label: string; centered?: boolean;
}) {
  const across = edge === "left" || edge === "right";
  const dimension = across ? "width" : "height";
  const property = `--panel-${dimension}`;
  const storageKey = `omniagent:panel-size:${resizeKey}:${dimension}`;
  const direction = edge === "left" || edge === "top" ? -1 : 1;
  const handle = useRef<HTMLDivElement>(null);
  const drag = useRef<Drag | undefined>(undefined);
  const [position, setPosition] = useState({ pixels: 0, percent: 0 });

  useLayoutEffect(() => {
    const panel = handle.current?.parentElement;
    if (!panel) return;
    panel.style.removeProperty(property);
    try {
      const saved = Number(localStorage.getItem(storageKey));
      if (Number.isFinite(saved) && saved > 0) panel.style.setProperty(property, `${saved}px`);
    } catch { /* The panel still works when local storage is unavailable. */ }
    const update = () => {
      const pixels = Math.round(panel.getBoundingClientRect()[dimension]);
      const available = panel.parentElement?.getBoundingClientRect()[dimension] || pixels || 1;
      const percent = Math.min(100, Math.round(pixels / available * 100));
      setPosition((before) => before.pixels === pixels && before.percent === percent ? before : { pixels, percent });
    };
    update();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(update);
    observer.observe(panel);
    if (panel.parentElement) observer.observe(panel.parentElement);
    return () => observer.disconnect();
  }, [dimension, property, storageKey]);

  function commit(panel: HTMLElement, size?: number) {
    if (size === undefined) panel.style.removeProperty(property);
    else panel.style.setProperty(property, `${Math.max(0, size)}px`);
    try {
      if (size === undefined) localStorage.removeItem(storageKey);
      else {
        // Persist the effective size after the layout has applied its minimum and maximum.
        const effective = Math.round(panel.getBoundingClientRect()[dimension]);
        if (effective > 0) {
          panel.style.setProperty(property, `${effective}px`);
          localStorage.setItem(storageKey, String(effective));
        }
      }
    } catch { /* Keep the new size for this mount. */ }
  }

  function coordinate(event: PointerEvent<HTMLDivElement>) { return across ? event.clientX : event.clientY; }

  function dragSize(event: PointerEvent<HTMLDivElement>, interaction: Drag) {
    const delta = (coordinate(event) - interaction.start) * direction * (centered ? 2 : 1);
    return Math.max(0, interaction.size + delta);
  }

  function start(event: PointerEvent<HTMLDivElement>) {
    const grip = event.currentTarget;
    const panel = grip.parentElement;
    if (!panel || drag.current || event.button !== 0) return;
    grip.focus({ preventScroll: true });
    event.preventDefault();
    grip.setPointerCapture(event.pointerId);
    grip.dataset.dragging = "true";
    drag.current = { pointer: event.pointerId, start: coordinate(event), size: panel.getBoundingClientRect()[dimension], previous: panel.style.getPropertyValue(property) };
  }

  function move(event: PointerEvent<HTMLDivElement>) {
    const interaction = drag.current;
    if (interaction?.pointer !== event.pointerId) return;
    event.currentTarget.parentElement?.style.setProperty(property, `${dragSize(event, interaction)}px`);
  }

  function stop(event: PointerEvent<HTMLDivElement>) {
    const interaction = drag.current;
    const grip = event.currentTarget;
    const panel = grip.parentElement;
    if (!panel || interaction?.pointer !== event.pointerId) return;
    drag.current = undefined;
    delete grip.dataset.dragging;
    if (event.type === "pointerup" && coordinate(event) !== interaction.start) commit(panel, dragSize(event, interaction));
    else panel.style.setProperty(property, interaction.previous);
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const panel = event.currentTarget.parentElement;
    if (!panel || drag.current) return;
    const backward = across ? "ArrowLeft" : "ArrowUp";
    const forward = across ? "ArrowRight" : "ArrowDown";
    if (![backward, forward, "Home", "End", "Enter"].includes(event.key)) return;
    event.preventDefault();
    if (event.key === "Enter") commit(panel);
    else if (event.key === "Home") commit(panel, 0);
    else if (event.key === "End") commit(panel, 100_000);
    else {
      const step = (event.shiftKey ? 40 : 16) * direction * (event.key === forward ? 1 : -1);
      commit(panel, panel.getBoundingClientRect()[dimension] + step);
    }
  }

  return <div ref={handle} className="panel-resize-handle" data-edge={edge} role="separator" tabIndex={0}
    aria-label={`Resize ${label} ${dimension}`} aria-orientation={across ? "vertical" : "horizontal"} aria-controls={panelId}
    aria-valuenow={position.percent} aria-valuetext={`${position.pixels} px`}
    title={`Resize ${label} (${dimension})`} onKeyDown={onKeyDown}
    onPointerDown={start} onPointerMove={move} onPointerUp={stop} onPointerCancel={stop} onLostPointerCapture={stop}
    onDoubleClick={(event) => { if (event.currentTarget.parentElement) commit(event.currentTarget.parentElement); }} />;
}
