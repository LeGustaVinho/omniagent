import { autocompletion } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, redo, undo } from "@codemirror/commands";
import { markdown } from "@codemirror/lang-markdown";
import { defaultHighlightStyle, syntaxHighlighting, syntaxTree } from "@codemirror/language";
import { openSearchPanel, search, searchKeymap } from "@codemirror/search";
import { Compartment, EditorSelection, EditorState } from "@codemirror/state";
import {
  Decoration, drawSelection, EditorView, highlightActiveLine, keymap, lineNumbers, ViewPlugin, WidgetType,
  type DecorationSet, type ViewUpdate,
} from "@codemirror/view";
import { Braces, CheckSquare, List, Redo2, Search, Table2, Undo2 } from "lucide-react";
import { useEffect, useRef } from "react";
import type { ExampleFile } from "@/ide-knowledge-data";
import { fileName } from "@/ide-knowledge-data";
import { parseWiki, type NoteTarget } from "@/knowledge-document";

export interface NoteJump {
  anchor?: string;
  line?: number;
  token: number;
}

const editorTheme = EditorView.theme({
  "&": { height: "100%", backgroundColor: "var(--background)", color: "var(--foreground)", fontSize: "13px" },
  ".cm-scroller": { fontFamily: '"Geist Mono", Consolas, monospace', lineHeight: "1.75", overflow: "auto" },
  ".cm-content": { padding: "16px 0 22px", caretColor: "var(--foreground)" },
  ".cm-line": { padding: "0 12px" },
  ".cm-gutters": { backgroundColor: "var(--background)", borderRight: "1px solid var(--border)", color: "var(--muted-foreground)" },
  ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "color-mix(in srgb, var(--muted) 65%, transparent)" },
  ".cm-panels": { backgroundColor: "var(--card)", color: "var(--foreground)" },
  ".cm-tooltip": { backgroundColor: "var(--card)", color: "var(--foreground)", border: "1px solid var(--border)" },
  ".cm-selectionBackground, &.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground": { backgroundColor: "color-mix(in srgb, var(--primary) 28%, transparent)" },
  "&.cm-focused": { outline: "none" },
  ".cm-note-link": { color: "var(--primary)", textDecoration: "underline", textUnderlineOffset: "2px" },
  ".cm-note-check": { accentColor: "var(--primary)", marginRight: "6px", verticalAlign: "middle" },
});

class ConcealedWiki extends WidgetType {
  constructor(readonly label: string, readonly target: string) { super(); }
  eq(other: ConcealedWiki) { return other.label === this.label && other.target === this.target; }
  toDOM() {
    const label = document.createElement("span");
    label.className = "cm-note-link";
    label.textContent = this.label;
    label.title = "Ctrl+click to follow " + this.target;
    return label;
  }
  ignoreEvent() { return false; }
}

class TaskMarker extends WidgetType {
  constructor(readonly checked: boolean, readonly valueAt: number) { super(); }
  eq(other: TaskMarker) { return other.checked === this.checked && other.valueAt === this.valueAt; }
  toDOM(view: EditorView) {
    const input = document.createElement("input");
    input.type = "checkbox";
    input.className = "cm-note-check";
    input.checked = this.checked;
    input.setAttribute("aria-label", "Toggle note task");
    input.addEventListener("change", () => view.dispatch({ changes: { from: this.valueAt, to: this.valueAt + 1, insert: input.checked ? "x" : " " } }));
    return input;
  }
  ignoreEvent() { return true; }
}

function liveDecorations(view: EditorView) {
  const marks: ReturnType<Decoration["range"]>[] = [];
  const selection = view.state.selection.main;
  const isActive = (from: number, to: number) => selection.from <= to && selection.to >= from;
  const hidden: { from: number; to: number }[] = [];
  for (const range of view.visibleRanges) {
    for (let at = range.from; at <= range.to;) {
      const line = view.state.doc.lineAt(at);
      if (!/^\s*(?:`{3,}|~{3,})/.test(line.text) && syntaxTree(view.state).resolveInner(line.from + (line.length ? 1 : 0)).name !== "CodeText") {
        for (const match of line.text.matchAll(/(!?)\[\[([^\]]+)\]\]/g)) {
          if (match.index === undefined || !match[2]) continue;
          const from = line.from + match.index;
          const to = from + match[0].length;
          if (isActive(from, to)) continue;
          const link = parseWiki(match[2], match[1] === "!");
          hidden.push({ from, to });
          marks.push(Decoration.replace({ widget: new ConcealedWiki(link.alias ?? (link.target || link.anchor || link.raw), link.raw) }).range(from, to));
        }
        const marker = /^\s*[-+*]\s+\[([ xX])\]/.exec(line.text);
        if (marker && !isActive(line.from, line.from + marker[0].length)) {
          const from = line.from + marker[0].lastIndexOf("[");
          const to = from + 3;
          hidden.push({ from, to });
          marks.push(Decoration.replace({ widget: new TaskMarker(marker[1]?.toLowerCase() === "x", from + 1) }).range(from, to));
        }
      }
      at = line.to + 1;
    }
    syntaxTree(view.state).iterate({
      from: range.from, to: range.to,
      enter({ name, from, to }) {
        if ((name === "EmphasisMark" || name === "CodeMark" || name === "HeaderMark")
          && !isActive(from, to) && !hidden.some((span) => from < span.to && to > span.from)) {
          marks.push(Decoration.replace({}).range(from, to));
        }
      },
    });
  }
  return Decoration.set(marks, true);
}

const livePreview = ViewPlugin.fromClass(class {
  decorations: DecorationSet;
  constructor(view: EditorView) { this.decorations = liveDecorations(view); }
  update(event: ViewUpdate) {
    if (event.docChanged || event.viewportChanged || event.selectionSet) this.decorations = liveDecorations(event.view);
  }
}, { decorations: (plugin) => plugin.decorations });

export function MarkdownEditor({ mode, path, source, files, jump, onChange, onLink }: {
  mode: "source" | "live";
  path: string;
  source: string;
  files: readonly ExampleFile[];
  jump?: NoteJump;
  onChange: (content: string) => void;
  onLink: (link: NoteTarget) => void;
}) {
  const mount = useRef<HTMLDivElement>(null);
  const instance = useRef<EditorView>(undefined);
  const compartment = useRef(new Compartment()).current;
  const fresh = useRef({ source, onChange, onLink, mode, files });
  const reflected = useRef(source);
  const updating = useRef(false);
  useEffect(() => { fresh.current = { source, onChange, onLink, mode, files }; }, [source, onChange, onLink, mode, files]);

  useEffect(() => {
    const target = mount.current;
    if (!target) return;
    reflected.current = fresh.current.source;
    const view = new EditorView({
      parent: target,
      state: EditorState.create({
        doc: fresh.current.source,
        extensions: [
          markdown(), lineNumbers(), highlightActiveLine(), drawSelection(), history(), search({ top: true }),
          keymap.of([...defaultKeymap, ...historyKeymap, ...searchKeymap]),
          syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
          editorTheme,
          compartment.of(fresh.current.mode === "live" ? livePreview : []),
          EditorView.contentAttributes.of({ "aria-label": `Markdown editor for ${path}` }),
          EditorView.lineWrapping,
          autocompletion({ override: [(context) => {
            const entered = context.matchBefore(/\[\[[^\]\n]*/);
            if (!entered) return null;
            const remaining = context.state.sliceDoc(context.pos, context.pos + 2);
            return {
              from: entered.from + 2,
              options: fresh.current.files.map(({ path: notePath }) => {
                const label = fileName(notePath).replace(/\.md$/i, "");
                return { label, detail: notePath, type: "text", apply: notePath + (remaining === "]]" ? "" : "]]") };
              }),
            };
          }] }),
          EditorView.updateListener.of((event) => {
            if (!event.docChanged || updating.current) return;
            const next = event.state.doc.toString();
            reflected.current = next;
            fresh.current.onChange(next);
          }),
          EditorView.domEventHandlers({
            mousedown(event, editor) {
              if (!event.ctrlKey && !event.metaKey) return false;
              const position = editor.posAtCoords({ x: event.clientX, y: event.clientY });
              if (position === null) return false;
              const line = editor.state.doc.lineAt(position);
              for (const link of line.text.matchAll(/(!?)\[\[([^\]]+)\]\]/g)) {
                const column = position - line.from;
                if (link.index !== undefined && link[2] && column >= link.index && column <= link.index + link[0].length) {
                  event.preventDefault();
                  fresh.current.onLink(parseWiki(link[2], link[1] === "!"));
                  return true;
                }
              }
              return false;
            },
          }),
        ],
      }),
    });
    instance.current = view;
    return () => { instance.current = undefined; view.destroy(); };
  }, [compartment, path]);

  useEffect(() => {
    instance.current?.dispatch({ effects: compartment.reconfigure(mode === "live" ? livePreview : []) });
  }, [compartment, mode]);

  useEffect(() => {
    const view = instance.current;
    if (!view || source === reflected.current) return;
    reflected.current = source;
    updating.current = true;
    try {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: source } });
    } finally {
      updating.current = false;
    }
  }, [source]);

  useEffect(() => {
    const view = instance.current;
    if (!view || !jump?.line) return;
    const lineNumber = Math.min(jump.line, view.state.doc.lines);
    const pos = view.state.doc.line(lineNumber).from;
    view.dispatch({ selection: EditorSelection.cursor(pos), effects: EditorView.scrollIntoView(pos, { y: "center" }) });
  }, [jump, path]);

  const insert = (prefix: string, suffix = "") => {
    const view = instance.current;
    if (!view) return;
    const selection = view.state.selection.main;
    const value = view.state.sliceDoc(selection.from, selection.to);
    view.dispatch({
      changes: { from: selection.from, to: selection.to, insert: prefix + value + suffix },
      selection: EditorSelection.cursor(selection.from + prefix.length + value.length),
    });
    view.focus();
  };

  return (
    <div className="note-editor">
      <div className="note-editor-tools" aria-label="Editing commands">
        <button type="button" className="fw-icon" title="Undo" aria-label="Undo" onClick={() => { if (instance.current) undo(instance.current); }}><Undo2 size={15} /></button>
        <button type="button" className="fw-icon" title="Redo" aria-label="Redo" onClick={() => { if (instance.current) redo(instance.current); }}><Redo2 size={15} /></button>
        <button type="button" className="fw-icon" title="Find in note" aria-label="Find in note" onClick={() => { if (instance.current) openSearchPanel(instance.current); }}><Search size={15} /></button>
        <button type="button" className="fw-icon" title="Insert list item" aria-label="Insert list item" onClick={() => insert("\n- ")}><List size={15} /></button>
        <button type="button" className="fw-icon" title="Insert task" aria-label="Insert task" onClick={() => insert("\n- [ ] ")}><CheckSquare size={15} /></button>
        <button type="button" className="fw-icon" title="Insert table" aria-label="Insert table" onClick={() => insert("\n| Name | Status |\n| --- | --- |\n|  |  |\n")}><Table2 size={15} /></button>
        <button type="button" className="fw-icon" title="Insert code block" aria-label="Insert code block" onClick={() => insert("\n```\n", "\n```\n")}><Braces size={15} /></button>
      </div>
      <div className="note-editor-mount" ref={mount} />
    </div>
  );
}
