import { useCallback, useRef, useState } from "react";

type TerminalPane = { id: string; directory: string; title: string; autoStart: boolean; clearRevision: number; generation?: number;
  reviewedInput?: string };
type TerminalLayout = { panes: TerminalPane[]; visible: string[]; active: string };

const initialLayout = (generation = 0): TerminalLayout => ({
  panes: [{ id: "", directory: "", title: "Checkout root", autoStart: false, clearRevision: 0, generation }], visible: [""], active: "",
});

function selectPane(layout: TerminalLayout, id: string): TerminalLayout {
  if (layout.active === id || !layout.panes.some((pane) => pane.id === id)) return layout;
  return {
    ...layout, active: id,
    visible: layout.visible.includes(id) ? layout.visible : layout.visible.map((visible) => visible === layout.active ? id : visible),
  };
}

export function useTerminalSessions() {
  const [layout, setLayout] = useState(initialLayout);
  const sequence = useRef(0);
  const openDirectory = useCallback((directory: string) => setLayout((layout) => {
    const exists = layout.panes.some(({ id }) => id === directory);
    const panes = exists ? layout.panes.map((pane) => pane.id === directory ? { ...pane, autoStart: true } : pane)
      : [...layout.panes, { id: directory, directory, title: directory || "Checkout root", autoStart: true, clearRevision: 0 }];
    return selectPane({ ...layout, panes }, directory);
  }), []);
  const openReview = useCallback((id: string, text: string) => setLayout((layout) => {
    if (layout.panes.some((pane) => pane.id === id)) return selectPane(layout, id);
    const panes = [...layout.panes, { id, directory: "", title: "PowerShell review", autoStart: false, clearRevision: 0, reviewedInput: text }];
    return selectPane({ ...layout, panes }, id);
  }), []);

  return {
    ...layout,
    openDirectory,
    openReview,
    select: (id: string) => setLayout((layout) => selectPane(layout, id)),
    duplicate(source: string, split: boolean) {
      const number = ++sequence.current;
      const id = `omni-terminal:${number}`;
      setLayout((layout) => {
        const pane = layout.panes.find((pane) => pane.id === source);
        if (!pane) return layout;
        const panes = [...layout.panes, { id, directory: pane.directory,
          title: `${pane.directory || "Checkout root"} (${number + 1})`, autoStart: true, clearRevision: 0 }];
        return split ? { panes, visible: [source, id], active: id } : selectPane({ ...layout, panes }, id);
      });
    },
    close(id: string) {
      const generation = ++sequence.current;
      setLayout((layout) => {
        const panes = layout.panes.filter((pane) => pane.id !== id);
        if (!panes.length) return initialLayout(generation);
        const visible = layout.visible.filter((pane) => pane !== id);
        const active = layout.active === id ? visible[0] ?? panes[0]!.id : layout.active;
        return { panes, active, visible: visible.length ? visible : [active] };
      });
    },
    rename: (id: string, title: string) => setLayout((layout) => ({ ...layout,
      panes: layout.panes.map((pane) => pane.id === id ? { ...pane, title } : pane),
    })),
    clear: () => setLayout((layout) => ({ ...layout,
      panes: layout.panes.map((pane) => pane.id === layout.active ? { ...pane, clearRevision: pane.clearRevision + 1 } : pane),
    })),
  };
}
