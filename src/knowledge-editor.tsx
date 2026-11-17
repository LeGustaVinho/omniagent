import { autocompletion, type Completion } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, redo, undo } from "@codemirror/commands";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { codeFolding, defaultHighlightStyle, ensureSyntaxTree, foldable, foldEffect, foldedRanges, syntaxHighlighting, syntaxTree, unfoldEffect } from "@codemirror/language";
import { openSearchPanel, search, searchKeymap } from "@codemirror/search";
import { Compartment, EditorSelection, EditorState, StateEffect, StateField, Transaction } from "@codemirror/state";
import {
  Decoration, drawSelection, EditorView, highlightActiveLine, keymap, lineNumbers, ViewPlugin, WidgetType,
  type Command, type DecorationSet, type ViewUpdate,
} from "@codemirror/view";
import { Braces, CheckSquare, List, Redo2, Search, Table2, Undo2 } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef } from "react";
import type { MenuGroups } from "@/components/ui/context-actions";
import type { ExampleFile } from "@/ide-knowledge-data";
import { fileName } from "@/ide-knowledge-data";
import { headings, markdownNoteLink, parseReadingFile, parseWiki, readingLocalAddress, type NoteTarget } from "@/knowledge-document";
import { isContextKey, useNoteContextMenu, type NoteContextActions, type NoteContextHit } from "@/knowledge-context";
import { useFileEditorDocument } from "@/file-editor-popout";
import { noteAttachmentCandidates } from "@/repository-note-media";
import { canFormatInlineMarkdown, canFormatMarkdown, insertCodeBlock, insertListItem, insertMarkdownLink, insertTable,
  markdownHeadingLevel, setMarkdownHeading, surroundInlineMarkdown, toggleTaskCheckbox } from "@/knowledge-source-commands";

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
  constructor(readonly label: string, readonly target: string, readonly start: number) { super(); }
  eq(other: ConcealedWiki) { return other.label === this.label && other.target === this.target && other.start === this.start; }
  toDOM() {
    const label = document.createElement("span");
    label.className = "cm-note-link";
    label.textContent = this.label;
    label.title = "Ctrl+click to follow " + this.target;
    label.dataset.noteOffset = String(this.start);
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
          marks.push(Decoration.replace({ widget: new ConcealedWiki(link.alias ?? (link.target || link.anchor || link.raw), link.raw, from) }).range(from, to));
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

type SourceRange = { from: number; to: number };
type SyntaxNode = {
  kind: string;
  range: SourceRange;
  markers?: SourceRange[];
  target?: string;
  alias?: string | null;
  anchor?: string | null;
  status?: string;
  checked?: boolean;
  valueRange?: SourceRange;
  level?: number;
  text?: string;
  id?: string;
  name?: string;
  language?: string;
};
type CanonicalSyntax = { bufferRevision: string; nodes: SyntaxNode[] };
const analyzed = StateEffect.define<CanonicalSyntax>();
const canonicalSyntax = StateField.define<CanonicalSyntax | undefined>({
  create: () => undefined,
  update(previous, transaction) {
    const analysis = transaction.effects.find((effect) => effect.is(analyzed));
    return analysis?.value ?? (transaction.docChanged ? undefined : previous);
  },
});
const formatted = {
  emphasis: Decoration.mark({ class: "cm-note-emphasis" }),
  strong: Decoration.mark({ class: "cm-note-strong" }),
  strikethrough: Decoration.mark({ class: "cm-note-strike" }),
  highlight: Decoration.mark({ class: "cm-note-highlight" }),
  code_span: Decoration.mark({ class: "cm-note-code" }),
};

function canonicalDecorations(view: EditorView): DecorationSet {
  const syntax = view.state.field(canonicalSyntax);
  if (!syntax) return Decoration.none;
  const decorations: ReturnType<Decoration["range"]>[] = [];
  const { doc, selection } = view.state;
  const conceal = Decoration.replace({});
  for (const node of syntax.nodes) {
    const { from, to } = node.range;
    if (from < 0 || from >= to || to > doc.length || !view.visibleRanges.some((range) => range.from < to && range.to > from)) continue;
    const active = selection.ranges.some((range) => range.from <= to && range.to >= from);
    if (node.kind === "heading") {
      const start = doc.lineAt(from).from;
      decorations.push(Decoration.line({ class: `cm-note-heading cm-note-heading-${node.level ?? 1}` }).range(start));
    }
    const formatting = formatted[node.kind as keyof typeof formatted];
    if (formatting) decorations.push(formatting.range(from, to));
    if (active) continue;
    if (node.kind === "wikilink" || node.kind === "embed") {
      const label = node.alias || node.target || node.anchor || "";
      if (label) decorations.push(Decoration.replace({ widget: new ConcealedWiki(label, node.target ?? label, from) }).range(from, to));
    } else if (node.kind === "task_marker" && node.valueRange && [" ", "x", "X"].includes(node.status ?? "")) {
      decorations.push(Decoration.replace({ widget: new TaskMarker(Boolean(node.checked), node.valueRange.from) }).range(from, to));
    } else {
      for (const marker of node.markers ?? []) {
        if (marker.from >= from && marker.to <= to && marker.from < marker.to) {
          decorations.push(conceal.range(marker.from, marker.to));
        }
      }
    }
  }
  return Decoration.set(decorations, true);
}

const canonicalLivePreview = ViewPlugin.fromClass(class {
  decorations: DecorationSet;
  constructor(view: EditorView) { this.decorations = canonicalDecorations(view); }
  update(event: ViewUpdate) {
    if (event.docChanged || event.viewportChanged || event.selectionSet
      || event.startState.field(canonicalSyntax) !== event.state.field(canonicalSyntax)) {
      this.decorations = canonicalDecorations(event.view);
    }
  }
}, { decorations: (plugin) => plugin.decorations });

function noteContextAt(editor: EditorView, path: string, offset: number): NoteContextHit {
  const { state } = editor;
  const doc = state.doc;
  const line = doc.lineAt(offset);
  const nodes = state.field(canonicalSyntax)?.nodes;
  const inside = ({ range }: SyntaxNode) => range.from <= offset && offset <= range.to;
  const link = nodes?.find((node) => ["wikilink", "embed", "link", "image"].includes(node.kind) && inside(node));
  const heading = nodes?.find((node) => node.kind === "heading" && inside(node));
  const block = nodes?.find((node) => node.kind === "block_id" && node.range.from >= line.from && node.range.to <= line.to);
  const tag = nodes?.find((node) => node.kind === "tag" && inside(node));
  const mermaid = nodes?.find((node) => node.kind === "code_block" && node.language?.toLowerCase() === "mermaid" && inside(node))?.text;
  const hit: NoteContextHit = { path, line: line.number, anchor: heading?.text || (block?.id ? `^${block.id}` : undefined), tag: tag?.name, mermaid };
  if (heading && hit.anchor) {
    hit.anchorKind = "heading";
    hit.anchorLine = doc.lineAt(heading.range.from).number;
  } else if (block && hit.anchor) {
    hit.anchorKind = "block";
    hit.anchorLine = doc.lineAt(block.range.from).number;
  }
  if (link) {
    const sourceLine = doc.lineAt(link.range.from).number;
    if (link.kind === "wikilink" || link.kind === "embed") {
      hit.link = { target: link.target ?? "", alias: link.alias ?? undefined, anchor: link.anchor ?? undefined,
        raw: state.sliceDoc(link.range.from, link.range.to).replace(/^!?\[\[|\]\]$/g, ""), embed: link.kind === "embed",
        sourcePath: path, sourceLine, sourceOffset: link.range.from, syntax: "wiki" };
    } else if (link.target) {
      const reference = markdownNoteLink(path, link.target, sourceLine);
      if (reference) hit.link = { ...reference, sourceOffset: link.range.from,
        editableInline: /\]\(/.test(state.sliceDoc(link.range.from, link.range.to)) };
      const file = parseReadingFile(readingLocalAddress(path, link.target));
      const paths = file && noteAttachmentCandidates(file[1], file[0], false);
      if (paths?.length) hit.attachment = { paths, label: link.text || file![1] };
    }
  } else if (!nodes) {
    const tree = syntaxTree(state);
    const syntax = tree.resolveInner(offset, 1);
    const isCode = (node: typeof syntax): boolean => /^(?:FencedCode|CodeBlock|CodeText|InlineCode)$/.test(node.name) || Boolean(node.parent && isCode(node.parent));
    if (!isCode(syntax)) {
      for (const match of line.text.matchAll(/(?<!\\)(!?)\[\[([^\]]+)\]\]/g)) {
        const start = line.from + match.index;
        if (match[2] && offset >= start && offset <= start + match[0].length) {
          hit.link = { ...parseWiki(match[2], match[1] === "!"), sourcePath: path, sourceLine: line.number, sourceOffset: start, syntax: "wiki" };
          break;
        }
      }
      if (!hit.link) {
        for (let node: typeof syntax | null = syntax; node; node = node.parent) {
          if (node.name !== "Link" && node.name !== "Image") continue;
          const url = node.getChild("URL");
          if (url) {
            const destination = state.sliceDoc(url.from, url.to).replace(/^<|>$/g, "");
            const reference = markdownNoteLink(path, destination, doc.lineAt(node.from).number);
            if (reference) hit.link = { ...reference, sourceOffset: node.from, editableInline: /\]\(/.test(state.sliceDoc(node.from, node.to)) };
            const file = parseReadingFile(readingLocalAddress(path, destination));
            const paths = file && noteAttachmentCandidates(file[1], file[0], false);
            if (paths?.length) hit.attachment = { paths, label: file![1] };
          }
          break;
        }
      }
      const fallback = headings(doc.toString()).find((entry) => entry.line === line.number);
      if (fallback) {
        hit.anchor = fallback.anchor;
        hit.anchorKind = "heading";
        hit.anchorLine = fallback.line;
      }
    }
  }
  const { from, to } = state.selection.main;
  if (from < to && offset >= from && offset <= to) {
    const text = state.sliceDoc(from, to);
    hit.excerpt = { path, text, startLine: doc.lineAt(from).number,
      endLine: doc.lineAt(to - 1).number, language: "markdown" };
    hit.selection = { from, to, text, source: doc.toString() };
  }
  return hit;
}

function sourceSectionMenu(editor: EditorView, hit: NoteContextHit): MenuGroups {
  if (hit.anchorKind !== "heading" || !hit.anchorLine) return [];
  const { state } = editor;
  const { doc } = state;
  const header = doc.line(hit.anchorLine);
  const tree = ensureSyntaxTree(state, doc.length, 40);
  if (!tree) return [];
  let heading = tree.resolveInner(header.to, -1);
  while (heading.parent && !/^(?:ATX|Setext)Heading[1-6]$/.test(heading.name)) heading = heading.parent;
  if (!/^(?:ATX|Setext)Heading[1-6]$/.test(heading.name) || doc.lineAt(heading.from).number !== header.number) return [];
  const from = doc.lineAt(heading.to).to;
  const to = foldable(state, header.from, header.to)?.to;
  if (to === undefined || from >= to) return [];
  const range = { from, to };
  let folded = false;
  foldedRanges(state).between(from, to, (start, end) => { if (start === from && end === to) folded = true; });
  return [[{ label: folded ? "Expand section" : "Collapse section", run: () => {
    if (editor.state.doc !== doc || !editor.dom.isConnected) return;
    editor.dispatch({ effects: (folded ? unfoldEffect : foldEffect).of(range), selection: EditorSelection.cursor(header.from) });
  } }]];
}

function sourceFormattingMenu(editor: EditorView, offset: number, hit: NoteContextHit): MenuGroups {
  const doc = editor.state.doc;
  const range = hit.selection ?? { from: offset, to: offset };
  if (!canFormatMarkdown(editor.state, range)) return [];
  const command = (label: string, edit: Command, position = range) => ({ label, run: () => {
    if (editor.state.doc !== doc || !editor.dom.isConnected) return;
    editor.dispatch({ selection: EditorSelection.single(position.from, position.to) });
    edit(editor);
  } });
  const level = markdownHeadingLevel(editor.state, offset);
  const inline = !hit.link && !hit.attachment && !hit.tag && canFormatInlineMarkdown(editor.state, range);
  return [[{ label: "Format Markdown", items: [
    ...(inline ? [
      command("Bold", surroundInlineMarkdown("**")), command("Italic", surroundInlineMarkdown("*")),
      command("Strikethrough", surroundInlineMarkdown("~~")), command("Inline code", surroundInlineMarkdown("`")),
      command("Insert Markdown link", insertMarkdownLink),
    ] : []),
    ...(level !== undefined ? [{ label: "Heading", items: [
      ...(level ? [command("Paragraph", setMarkdownHeading(0), { from: offset, to: offset })] : []),
      ...[1, 2, 3, 4, 5, 6].filter((number) => number !== level)
        .map((number) => command(`Heading ${number}`, setMarkdownHeading(number), { from: offset, to: offset })),
    ] }] : []),
    command("Insert list item", insertListItem), command("Toggle task checkbox", toggleTaskCheckbox),
    command("Insert table", insertTable), command("Insert code block", insertCodeBlock),
  ] }]];
}

export function MarkdownEditor({ mode, path, source, files, jump, onChange, onLink, analyze = false, suggestLinks, contextActions }: {
  mode: "source" | "live";
  path: string;
  source: string;
  files: readonly ExampleFile[];
  jump?: NoteJump;
  onChange: (content: string) => void;
  onLink: (link: NoteTarget) => void;
  analyze?: boolean;
  suggestLinks?: (prefix: string) => Promise<Completion[]>;
  contextActions?: NoteContextActions;
}) {
  const mount = useRef<HTMLDivElement>(null);
  const instance = useRef<EditorView>(undefined);
  const owner = useFileEditorDocument();
  const compartment = useRef(new Compartment()).current;
  const { open: openContextMenu, overlay } = useNoteContextMenu();
  const fresh = useRef({ source, onChange, onLink, mode, files, analyze, suggestLinks, contextActions });
  const generation = useRef(0);
  const reflected = useRef(source);
  const updating = useRef(false);
  useEffect(() => { fresh.current = { source, onChange, onLink, mode, files, analyze, suggestLinks, contextActions }; }, [source, onChange, onLink, mode, files, analyze, suggestLinks, contextActions]);

  useEffect(() => {
    const target = mount.current;
    if (!target) return;
    reflected.current = fresh.current.source;
    const show = (editor: EditorView, pos: number, point: { x: number; y: number }) => {
      if (!fresh.current.contextActions) return false;
      const hit = noteContextAt(editor, path, pos);
      const groups = [...fresh.current.contextActions(hit), ...sourceSectionMenu(editor, hit), ...sourceFormattingMenu(editor, pos, hit)];
      return openContextMenu(hit.link?.alias || hit.link?.target || hit.attachment?.label || hit.anchor || hit.tag || "Note source", groups, point, () => {
        if (editor.dom.isConnected && !editor.dom.closest("[hidden]")) editor.focus();
      });
    };
    const view = new EditorView({
      parent: target,
      root: target.ownerDocument,
      state: EditorState.create({
        doc: fresh.current.source,
        extensions: [
          markdown({ base: markdownLanguage }), codeFolding(), lineNumbers(), highlightActiveLine(), drawSelection(), history(), search({ top: true }),
          keymap.of([...defaultKeymap, ...historyKeymap, ...searchKeymap]),
          syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
          editorTheme,
          canonicalSyntax,
          compartment.of(fresh.current.mode === "live" ? (fresh.current.analyze ? canonicalLivePreview : livePreview) : []),
          EditorView.contentAttributes.of({ "aria-label": `Markdown editor for ${path}` }),
          EditorView.lineWrapping,
          autocompletion({ override: [async (context) => {
            const entered = context.matchBefore(/\[\[[^\]\n]*/);
            if (!entered) return null;
            const remaining = context.state.sliceDoc(context.pos, context.pos + 2);
            if (fresh.current.suggestLinks) {
              try {
                const matches = await fresh.current.suggestLinks(entered.text.slice(2));
                if (context.aborted) return null;
                return { from: entered.from + 2, filter: false, options: matches.map((match) => ({
                  ...match, apply: (typeof match.apply === "string" ? match.apply : match.label) + (remaining === "]]" ? "" : "]]"),
                })) };
              } catch { return null; }
            }
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
            contextmenu(event, editor) {
              const target = event.target as Element | null;
              const offset = target?.closest?.<HTMLElement>("[data-note-offset]")?.dataset.noteOffset;
              const pos = offset === undefined ? editor.posAtCoords({ x: event.clientX, y: event.clientY }) : Number(offset);
              return pos !== null && show(editor, pos, { x: event.clientX, y: event.clientY });
            },
            keydown(event, editor) {
              if (!isContextKey(event)) return false;
              const { main } = editor.state.selection;
              const caret = editor.coordsAtPos(main.head);
              const bounds = editor.dom.getBoundingClientRect();
              return show(editor, main.head, { x: caret?.left ?? bounds.left, y: caret?.bottom ?? bounds.top });
            },
            mousedown(event, editor) {
              if (!event.ctrlKey && !event.metaKey) return false;
              const position = editor.posAtCoords({ x: event.clientX, y: event.clientY });
              if (position === null) return false;
              if (fresh.current.analyze) {
                const link = editor.state.field(canonicalSyntax)?.nodes.find((node) => (node.kind === "wikilink" || node.kind === "embed")
                  && position >= node.range.from && position <= node.range.to);
                if (!link) return false;
                event.preventDefault();
                fresh.current.onLink({
                  target: link.target ?? "", alias: link.alias ?? undefined, anchor: link.anchor ?? undefined,
                  raw: editor.state.sliceDoc(link.range.from, link.range.to).replace(/^!?\[\[|\]\]$/g, ""), embed: link.kind === "embed",
                  sourceLine: editor.state.doc.lineAt(link.range.from).number,
                });
                return true;
              }
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
  }, [compartment, openContextMenu, path]);

  useEffect(() => {
    if (owner && instance.current?.root !== owner) instance.current?.setRoot(owner);
  }, [owner]);

  useEffect(() => {
    instance.current?.dispatch({ effects: compartment.reconfigure(mode === "live" ? (analyze ? canonicalLivePreview : livePreview) : []) });
  }, [analyze, compartment, mode]);

  useEffect(() => {
    const view = instance.current;
    if (!view || source === reflected.current) return;
    reflected.current = source;
    updating.current = true;
    try {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: source }, annotations: Transaction.addToHistory.of(false) });
    } finally {
      updating.current = false;
    }
  }, [source]);

  useEffect(() => {
    if (!analyze) return;
    const bufferRevision = String(++generation.current);
    const markdown = source.replace(/\r\n?/g, "\n");
    const timer = window.setTimeout(() => {
      void invoke<CanonicalSyntax>("analyze_note_markdown", { markdown: source, bufferRevision }).then((analysis) => {
        const editor = instance.current;
        if (String(generation.current) !== bufferRevision || analysis.bufferRevision !== bufferRevision
          || !editor || editor.state.doc.toString() !== markdown) return;
        editor.dispatch({ effects: analyzed.of(analysis) });
      }).catch(() => undefined);
    }, 75);
    return () => {
      ++generation.current;
      window.clearTimeout(timer);
    };
  }, [analyze, path, source]);

  useEffect(() => {
    const view = instance.current;
    if (!view || !jump?.line) return;
    const lineNumber = Math.min(jump.line, view.state.doc.lines);
    const pos = view.state.doc.line(lineNumber).from;
    view.dispatch({ selection: EditorSelection.cursor(pos), effects: EditorView.scrollIntoView(pos, { y: "center" }) });
  }, [jump, path]);

  const runEdit = (edit: Command) => {
    const view = instance.current;
    if (!view) return;
    edit(view);
    view.focus();
  };

  return (
    <div className="note-editor" data-mode={mode}>
      <div className="note-editor-tools" aria-label="Editing commands">
        <button type="button" className="fw-icon" title="Undo" aria-label="Undo" onClick={() => { if (instance.current) undo(instance.current); }}><Undo2 size={15} /></button>
        <button type="button" className="fw-icon" title="Redo" aria-label="Redo" onClick={() => { if (instance.current) redo(instance.current); }}><Redo2 size={15} /></button>
        <button type="button" className="fw-icon" title="Find in note" aria-label="Find in note" onClick={() => { if (instance.current) openSearchPanel(instance.current); }}><Search size={15} /></button>
        <button type="button" className="fw-icon" title="Insert list item" aria-label="Insert list item" onClick={() => runEdit(insertListItem)}><List size={15} /></button>
        <button type="button" className="fw-icon" title="Toggle task checkbox" aria-label="Toggle task checkbox" onClick={() => runEdit(toggleTaskCheckbox)}><CheckSquare size={15} /></button>
        <button type="button" className="fw-icon" title="Insert table" aria-label="Insert table" onClick={() => runEdit(insertTable)}><Table2 size={15} /></button>
        <button type="button" className="fw-icon" title="Insert code block" aria-label="Insert code block" onClick={() => runEdit(insertCodeBlock)}><Braces size={15} /></button>
      </div>
      <div className="note-editor-mount" ref={mount} />
      {overlay}
    </div>
  );
}
