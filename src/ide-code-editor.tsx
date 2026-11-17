import type { editor } from "monaco-editor";
import { useEffect, useRef, useState } from "react";
import { PositionedContextActions, type MenuAction, type MenuGroups } from "@/components/ui/context-actions";
import { excerptReference, markdownExcerpt, type EditorExcerpt } from "@/editor-context";
import { useFileEditorDocument } from "@/file-editor-popout";
import type { GitPermalinkLines } from "@/git-file-versions";
import { GitLineBlameDialog, type GitLineBlameRequest } from "@/git-line-blame";
import { isProjectSearchText } from "@/ide-file-search";
import { loadMonaco } from "@/ide-monaco";

export interface CodeProblem {
  message: string;
  line: number;
  column: number;
  label: "Error" | "Warning" | "Information";
  quickFix?: boolean;
}

export type EditorLocation = { line: number; column?: number };
export type EditorSettings = {
  language: string;
  autoLanguage: string;
  languages: readonly { id: string; label: string }[];
  tabSize: number;
  insertSpaces: boolean;
  setLanguage: (id: string) => void;
  setIndentation: (tabSize: number, insertSpaces: boolean) => void;
  detectIndentation: () => void;
};

export interface CodeModel {
  model: editor.ITextModel;
  onChange: ReturnType<editor.ITextModel["onDidChangeContent"]>;
  synchronizing: boolean;
  autoLanguage: string;
}

const quickFixId = "editor.action.quickFix";
const duplicateLineId = "editor.action.copyLinesDownAction";
const duplicateSelectionId = "editor.action.duplicateSelection";
const toggleCommentId = "editor.action.commentLine";
const findSelectionId = "actions.findWithSelection";

export function CodeEditor({ scope, path, content, models, update, problems, jump, quickFix, onQuickFixShown, onSettings, onCopy, onExcerpt, onPermalink, onOmniAgentLink, onSearchText, onOpenLocation, blameCheckoutId }: {
  scope: string;
  path: string;
  content: string;
  models: Map<string, CodeModel>;
  update: (path: string, content: string) => void;
  problems: (path: string, markers: CodeProblem[]) => void;
  jump?: EditorLocation & { token: number };
  quickFix?: EditorLocation;
  onQuickFixShown?: () => void;
  onSettings?: (path: string, options: EditorSettings) => void;
  onCopy?: (text: string, status: string) => void;
  onExcerpt?: (selection: EditorExcerpt, instruction?: string) => void;
  onPermalink?: (path: string, lines: GitPermalinkLines) => void;
  onOmniAgentLink?: (path: string, line: number, column: number) => void;
  onSearchText?: (text: string) => void;
  onOpenLocation?: (path: string, location?: EditorLocation) => Promise<boolean>;
  blameCheckoutId?: string;
}) {
  const mount = useRef<HTMLDivElement>(null);
  const instance = useRef<editor.IStandaloneCodeEditor>(undefined);
  const owner = useFileEditorDocument();
  const viewState = useRef<{ path: string; scope: string; jump?: number; state: editor.ICodeEditorViewState | null }>(undefined);
  const fresh = useRef({ content, update, problems, jump, onSettings, onCopy, onExcerpt, onPermalink, onOmniAgentLink, onSearchText, onOpenLocation });
  const [error, setError] = useState("");
  const [ready, setReady] = useState(false);
  const [blame, setBlame] = useState<GitLineBlameRequest>();
  const [gutterMenu, setGutterMenu] = useState<{ path: string; line: number; groups: MenuGroups; point: { x: number; y: number } }>();
  const gutterRestoreFocus = useRef(true);
  useEffect(() => {
    fresh.current = { content, update, problems, jump, onSettings, onCopy, onExcerpt, onPermalink, onOmniAgentLink, onSearchText, onOpenLocation };
  }, [content, update, problems, jump, onSettings, onCopy, onExcerpt, onPermalink, onOmniAgentLink, onSearchText, onOpenLocation]);

  useEffect(() => {
    if (!mount.current) return;
    let cancelled = false;
    const subscriptions: { dispose: () => void }[] = [];
    const target = mount.current;

    void loadMonaco().then((monaco) => {
      if (cancelled) return;
      const uri = monaco.Uri.from({ scheme: "omni-source", path: `/${scope}/${path}` });
      let record = models.get(path);
      if (!record) {
        const language = /\.toml$/i.test(path) ? "ini" : undefined;
        const model = monaco.editor.createModel(fresh.current.content, language, uri);
        record = {
          model,
          autoLanguage: model.getLanguageId(),
          synchronizing: false,
          onChange: model.onDidChangeContent(() => {
            if (!models.get(path)?.synchronizing) fresh.current.update(path, model.getValue());
          }),
        };
        models.set(path, record);
      }
      const { model, autoLanguage } = record;
      const codeEditor = monaco.editor.create(target, {
        model,
        automaticLayout: true,
        minimap: { enabled: false },
        scrollbar: { verticalScrollbarSize: 10, horizontalScrollbarSize: 10 },
        scrollBeyondLastLine: false,
        fontFamily: '"Geist Mono Variable", Consolas, monospace',
        fontSize: 13,
        lineHeight: 22,
        renderLineHighlight: "line",
        padding: { top: 10, bottom: 10 },
        ariaLabel: `Source editor for ${path}`,
        tabSize: 2,
      });
      instance.current = codeEditor;
      if (fresh.current.onOpenLocation) {
        const prefix = `/${scope}/`;
        subscriptions.push(monaco.editor.registerEditorOpener({
          openCodeEditor(source, resource, position) {
            if (cancelled || source !== codeEditor || resource.scheme !== uri.scheme || resource.authority || resource.query || resource.fragment
              || resource.path === uri.path || !resource.path.startsWith(prefix)) return false;
            const destination = resource.path.slice(prefix.length);
            if (/[\\:*?"<>|\u0000-\u001F\u007F]/u.test(destination) || destination.split("/").some((part) =>
              !part || part.endsWith(".") || part.endsWith(" ") || part.toLowerCase() === ".git")) return false;
            const location = position && ("startLineNumber" in position
              ? { line: position.startLineNumber, column: position.startColumn }
              : { line: position.lineNumber, column: position.column });
            if (location && (!Number.isSafeInteger(location.line) || location.line < 1
              || !Number.isSafeInteger(location.column) || location.column < 1)) return false;
            return fresh.current.onOpenLocation?.(destination, location) ?? false;
          },
        }));
      }
      if (fresh.current.onSettings) {
        const languages = monaco.languages.getLanguages().map(({ id, aliases }) => ({ id, label: aliases?.[0] ?? id }))
          .sort((left, right) => left.label.localeCompare(right.label));
        function publishSettings() {
          const { tabSize, insertSpaces } = model.getOptions();
          fresh.current.onSettings?.(path, { language: model.getLanguageId(), autoLanguage, languages, tabSize, insertSpaces,
            setLanguage: (id) => monaco.editor.setModelLanguage(model, id),
            setIndentation: (tabSize, insertSpaces) => model.updateOptions({ tabSize, indentSize: tabSize, insertSpaces }),
            detectIndentation: () => model.detectIndentation(true, 2),
          });
        }
        subscriptions.push(model.onDidChangeLanguage(publishSettings), model.onDidChangeOptions(publishSettings));
        publishSettings();
      }
      function excerpt(allowEmpty: boolean): EditorExcerpt | undefined {
        const selection = codeEditor.getSelection();
        const model = codeEditor.getModel();
        if (!selection || !model || (!allowEmpty && selection.isEmpty())) return;
        const { startLineNumber, endLineNumber, endColumn } = selection;
        const language = model.getLanguageId();
        return { path, text: model.getValueInRange(selection), startLine: startLineNumber,
          endLine: endLineNumber > startLineNumber && endColumn === 1 ? endLineNumber - 1 : endLineNumber,
          language: language === "plaintext" ? undefined : language };
      }
      function addCommand(id: string, label: string, command: string, order: number, precondition: string) {
        const action = codeEditor.getAction(command);
        if (!action) return;
        subscriptions.push(codeEditor.addAction({
          id: `omniagent.${id}`, label, precondition, contextMenuGroupId: "8_omniagent-edit", contextMenuOrder: order,
          run: () => { if (action.isSupported()) return action.run(); },
        }));
      }
      addCommand("duplicate-line", "Duplicate line", duplicateLineId, 1, "!editorReadonly");
      addCommand("duplicate-selection", "Duplicate selection", duplicateSelectionId, 2, "!editorReadonly && editorHasSelection");
      addCommand("toggle-line-comment", "Toggle line comment", toggleCommentId, 3, "!editorReadonly && editorLangId != plaintext");
      addCommand("find-selection", "Find selection in this file", findSelectionId, 4, "editorHasSelection");
      if (fresh.current.onSearchText) {
        const canSearch = codeEditor.createContextKey<boolean>("omniagentCanSearchSelection", false);
        function updateSearchAvailability() {
          const selection = codeEditor.getSelection();
          canSearch.set(Boolean(selection && !selection.isEmpty()
            && selection.startLineNumber === selection.endLineNumber
            && selection.endColumn - selection.startColumn <= 4096
            && isProjectSearchText(model.getValueInRange(selection))));
        }
        subscriptions.push(codeEditor.onDidChangeCursorSelection(updateSearchAvailability),
          model.onDidChangeContent(updateSearchAvailability), codeEditor.addAction({
            id: "omniagent.find-selection-in-project", label: "Find selection in project",
            precondition: "omniagentCanSearchSelection", contextMenuGroupId: "8_omniagent-edit", contextMenuOrder: 5,
            run: () => {
              const range = excerpt(false);
              if (range && isProjectSearchText(range.text)) fresh.current.onSearchText?.(range.text);
            },
          }));
        updateSearchAvailability();
      }
      if (fresh.current.onCopy) {
        codeEditor.addAction({
          id: "omniagent.copy-source-reference", label: "Copy file and line reference", contextMenuGroupId: "9_omniagent-copy", contextMenuOrder: 1,
          run: () => { const range = excerpt(true); if (range) fresh.current.onCopy?.(excerptReference(range), "Copied source reference"); },
        });
        codeEditor.addAction({
          id: "omniagent.copy-selection-markdown", label: "Copy selection as Markdown", precondition: "editorHasSelection",
          contextMenuGroupId: "9_omniagent-copy", contextMenuOrder: 2,
          run: () => { const range = excerpt(false); if (range) fresh.current.onCopy?.(markdownExcerpt(range), "Copied Markdown code block"); },
        });
      }
      if (fresh.current.onOmniAgentLink) codeEditor.addAction({
        id: "omniagent.copy-file-link", label: "Copy OmniAgent link to line",
        contextMenuGroupId: "9_omniagent-copy", contextMenuOrder: 3,
        run: () => {
          const location = codeEditor.getPosition();
          if (location) fresh.current.onOmniAgentLink?.(path, location.lineNumber, location.column);
        },
      });
      if (fresh.current.onPermalink) codeEditor.addAction({
        id: "omniagent.copy-committed-line-link", label: "Copy committed line permalink",
        contextMenuGroupId: "9_omniagent-copy", contextMenuOrder: 4,
        run: () => {
          const range = excerpt(true);
          const model = codeEditor.getModel();
          if (!range || !model) return;
          const { startLine, endLine } = range;
          const text = model.getValueInRange({ startLineNumber: startLine, startColumn: 1,
            endLineNumber: endLine, endColumn: model.getLineMaxColumn(endLine) });
          fresh.current.onPermalink?.(path, { startLine, endLine, text });
        },
      });
      const showBlame = (line: number) => {
        if (blameCheckoutId) setBlame({ checkoutId: blameCheckoutId, path, line, editorText: model.getValue(monaco.editor.EndOfLinePreference.LF) });
      };
      if (blameCheckoutId) {
        let contextLine: number | undefined;
        const clearContextLine = () => { contextLine = undefined; };
        subscriptions.push(
          codeEditor.onContextMenu(({ target }) => { contextLine = target.position?.lineNumber; }),
          codeEditor.onMouseDown(clearContextLine), codeEditor.onKeyDown(clearContextLine),
        );
        codeEditor.addAction({
          id: "omniagent.git-line-blame", label: "Show Git blame for line…", contextMenuGroupId: "9_omniagent-git",
          run: () => {
            const line = contextLine ?? codeEditor.getPosition()?.lineNumber;
            clearContextLine();
            if (line !== undefined) showBlame(line);
          },
        });
      }
      if (fresh.current.onExcerpt) {
        const agentActions = [
          { id: "quote", label: "Add selection to conversation…" },
          { id: "explain", label: "Explain selection…", instruction: "Explain how this code works and what it depends on." },
          { id: "usages", label: "Find usages of selection…", instruction: "Find callers or references to the code in this selection. Cite the relevant file paths and lines." },
          { id: "review", label: "Review selection…", instruction: "Review this code for correctness and regressions." },
          { id: "fix", label: "Fix selection…", instruction: "Find and fix the issue in this code. Ask for the expected behavior if it is unclear." },
          { id: "test", label: "Write tests for selection…", instruction: "Write tests that cover the behavior and edge cases of this code." },
          { id: "document", label: "Document selection…", instruction: "Draft documentation for the behavior and usage of the selected code." },
          { id: "refactor", label: "Propose a refactor for selection…", instruction: "Propose a behavior-preserving refactor of the selected code and explain how to verify it." },
        ];
        agentActions.forEach(({ id, label, instruction }, order) => codeEditor.addAction({
          id: `omniagent.${id}-source-selection`, label, precondition: "editorHasSelection",
          contextMenuGroupId: "9_omniagent-context", contextMenuOrder: order,
          run: () => { const range = excerpt(false); if (range) fresh.current.onExcerpt?.(range, instruction); },
        }));
      }
      subscriptions.push(model.onDidChangeContent(() => setGutterMenu(undefined)), codeEditor.onContextMenu(({ event, target }) => {
        if (target.type !== monaco.editor.MouseTargetType.GUTTER_LINE_NUMBERS || !target.position) return;
        const line = target.position.lineNumber;
        const text = model.getLineContent(line);
        const language = model.getLanguageId();
        const range = { path, startLine: line, endLine: line, text, language: language === "plaintext" ? undefined : language };
        const edit: MenuAction[] = [];
        for (const [label, command] of [["Duplicate line", duplicateLineId], ["Toggle line comment", toggleCommentId]] as const) {
          const action = codeEditor.getAction(command);
          if (!action?.isSupported() || command === toggleCommentId && language === "plaintext") continue;
          edit.push({ label, run: () => {
            if (!action.isSupported()) return;
            codeEditor.setPosition({ lineNumber: line, column: 1 });
            void action.run();
          } });
        }
        const { onCopy, onExcerpt, onOmniAgentLink, onPermalink, onSearchText } = fresh.current;
        const groups: MenuGroups = [edit, [
          ...(onCopy ? [
            { label: "Copy line", run: () => onCopy(text, "Copied line") },
            { label: "Copy file and line reference", run: () => onCopy(excerptReference(range), "Copied source reference") },
          ] : []),
          ...(onOmniAgentLink ? [{ label: "Copy OmniAgent link to line", run: () => onOmniAgentLink(path, line, 1) }] : []),
          ...(onPermalink ? [{ label: "Copy committed line permalink", run: () => onPermalink(path, range) }] : []),
        ], [
          ...(onSearchText && isProjectSearchText(text) ? [{ label: "Find line in project", run: () => { gutterRestoreFocus.current = false; onSearchText(text); } }] : []),
          ...(onExcerpt && text ? [{ label: "Add line to conversation…", run: () => { gutterRestoreFocus.current = false; onExcerpt(range); } }] : []),
          ...(blameCheckoutId ? [{ label: "Show Git blame for line…", run: () => { gutterRestoreFocus.current = false; showBlame(line); } }] : []),
        ]];
        if (!groups.some((group) => group.length)) return;
        event.preventDefault();
        event.stopPropagation();
        gutterRestoreFocus.current = true;
        const { clientX: x, clientY: y } = event.browserEvent;
        setGutterMenu({ path, line, groups, point: { x, y } });
      }));
      function publishDiagnostics() {
        const quickFix = Boolean(codeEditor.getAction(quickFixId)?.isSupported());
        const entries = monaco.editor.getModelMarkers({ resource: uri }).map<CodeProblem>((marker) => ({
          message: marker.message,
          line: marker.startLineNumber,
          column: marker.startColumn,
          quickFix,
          label: marker.severity === monaco.MarkerSeverity.Error ? "Error"
            : marker.severity === monaco.MarkerSeverity.Warning ? "Warning" : "Information",
        }));
        fresh.current.problems(path, entries);
      }
      subscriptions.push(monaco.editor.onDidChangeMarkers((resources) => {
        if (resources.some((resource) => resource.toString() === uri.toString())) publishDiagnostics();
      }), model.onDidChangeLanguage(publishDiagnostics));
      publishDiagnostics();
      const location = fresh.current.jump;
      const previous = viewState.current;
      if (previous?.path === path && previous.scope === scope && previous.jump === location?.token && previous.state) {
        codeEditor.restoreViewState(previous.state);
      } else if (location) {
        codeEditor.setPosition({ lineNumber: location.line, column: location.column ?? 1 });
        codeEditor.revealLineInCenter(location.line);
      }
      setReady(true);
    }).catch((failure: unknown) => {
      if (!cancelled) setError(String(failure));
    });
    return () => {
      cancelled = true;
      subscriptions.forEach((subscription) => subscription.dispose());
      if (instance.current) viewState.current = { path, scope, jump: fresh.current.jump?.token, state: instance.current.saveViewState() };
      instance.current?.dispose();
      instance.current = undefined;
    };
  }, [blameCheckoutId, models, owner, path, scope]);

  useEffect(() => {
    const codeEditor = instance.current;
    if (jump && codeEditor) {
      codeEditor.setPosition({ lineNumber: jump.line, column: jump.column ?? 1 });
      codeEditor.revealLineInCenter(jump.line);
    }
  }, [jump]);
  useEffect(() => {
    const codeEditor = instance.current;
    if (!ready || !codeEditor || !quickFix) return;
    const frame = requestAnimationFrame(() => {
      const action = codeEditor.getAction(quickFixId);
      if (action?.isSupported()) {
        codeEditor.setPosition({ lineNumber: quickFix.line, column: quickFix.column ?? 1 });
        codeEditor.revealLineInCenter(quickFix.line);
        codeEditor.focus();
        void action.run();
      }
      onQuickFixShown?.();
    });
    return () => cancelAnimationFrame(frame);
  }, [ready, quickFix, onQuickFixShown]);
  return (
    <div className="code-editor">
      <div ref={mount} className="code-editor-target" />
      {!ready && <p role={error ? "alert" : "status"} className="file-help">{error || "Loading source editor…"}</p>}
      {gutterMenu?.path === path && <PositionedContextActions label={`${path}:${gutterMenu.line}`} groups={gutterMenu.groups}
        point={gutterMenu.point} container={mount.current?.ownerDocument.body} onClose={() => setGutterMenu(undefined)}
        restoreFocus={() => { if (gutterRestoreFocus.current) instance.current?.focus(); }} />}
      {blame && blame.checkoutId === blameCheckoutId && blame.path === path && <GitLineBlameDialog request={blame}
        close={() => setBlame(undefined)} restoreFocus={() => instance.current?.focus()} copy={onCopy} />}
    </div>
  );
}
