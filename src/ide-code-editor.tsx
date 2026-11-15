import type { editor } from "monaco-editor";
import { useEffect, useRef, useState } from "react";
import { loadMonaco, monacoTheme } from "@/ide-monaco";

export interface CodeProblem {
  message: string;
  line: number;
  column: number;
  label: "Error" | "Warning" | "Information";
}

export interface CodeModel {
  model: editor.ITextModel;
  onChange: ReturnType<editor.ITextModel["onDidChangeContent"]>;
  synchronizing: boolean;
}

function languageFor(path: string) {
  const extension = path.split(".").at(-1);
  const languages: Record<string, string> = {
    ts: "typescript", tsx: "typescript", js: "javascript", jsx: "javascript", json: "json",
    css: "css", html: "html", rs: "rust", toml: "ini", ps1: "powershell", txt: "plaintext",
  };
  return languages[extension ?? ""] ?? "plaintext";
}

export function CodeEditor({ scope, path, content, models, update, problems, jump }: {
  scope: string;
  path: string;
  content: string;
  models: Map<string, CodeModel>;
  update: (path: string, content: string) => void;
  problems: (path: string, markers: CodeProblem[]) => void;
  jump?: { line: number; token: number };
}) {
  const mount = useRef<HTMLDivElement>(null);
  const instance = useRef<editor.IStandaloneCodeEditor>(undefined);
  const fresh = useRef({ content, update, problems, jump });
  const [error, setError] = useState("");
  const [ready, setReady] = useState(false);
  useEffect(() => { fresh.current = { content, update, problems, jump }; }, [content, update, problems, jump]);

  useEffect(() => {
    if (!mount.current) return;
    let cancelled = false;
    let observer: MutationObserver | undefined;
    let diagnosticSubscription: { dispose: () => void } | undefined;
    const target = mount.current;

    void loadMonaco().then((monaco) => {
      if (cancelled) return;
      const uri = monaco.Uri.from({ scheme: "omni-source", path: `/${scope}/${path}` });
      let record = models.get(path);
      if (!record) {
        const model = monaco.editor.createModel(fresh.current.content, languageFor(path), uri);
        record = {
          model,
          synchronizing: false,
          onChange: model.onDidChangeContent(() => {
            if (!models.get(path)?.synchronizing) fresh.current.update(path, model.getValue());
          }),
        };
        models.set(path, record);
      }
      const codeEditor = monaco.editor.create(target, {
        model: record.model,
        theme: monacoTheme(),
        automaticLayout: true,
        minimap: { enabled: false },
        scrollbar: { verticalScrollbarSize: 10, horizontalScrollbarSize: 10 },
        scrollBeyondLastLine: false,
        fontFamily: '"Geist Mono", Consolas, monospace',
        fontSize: 13,
        lineHeight: 22,
        renderLineHighlight: "line",
        padding: { top: 10, bottom: 10 },
        ariaLabel: `Source editor for ${path}`,
        tabSize: 2,
      });
      instance.current = codeEditor;
      function publishDiagnostics() {
        const entries = monaco.editor.getModelMarkers({ resource: uri }).map<CodeProblem>((marker) => ({
          message: marker.message,
          line: marker.startLineNumber,
          column: marker.startColumn,
          label: marker.severity === monaco.MarkerSeverity.Error ? "Error"
            : marker.severity === monaco.MarkerSeverity.Warning ? "Warning" : "Information",
        }));
        fresh.current.problems(path, entries);
      }
      diagnosticSubscription = monaco.editor.onDidChangeMarkers((resources) => {
        if (resources.some((resource) => resource.toString() === uri.toString())) publishDiagnostics();
      });
      publishDiagnostics();
      observer = new MutationObserver(() => monaco.editor.setTheme(monacoTheme()));
      observer.observe(document.documentElement, { attributes: true });
      const location = fresh.current.jump;
      if (location) {
        codeEditor.setPosition({ lineNumber: location.line, column: 1 });
        codeEditor.revealLineInCenter(location.line);
      }
      setReady(true);
    }).catch((failure: unknown) => {
      if (!cancelled) setError(String(failure));
    });
    return () => {
      cancelled = true;
      observer?.disconnect();
      diagnosticSubscription?.dispose();
      instance.current?.dispose();
      instance.current = undefined;
    };
  }, [models, path, scope]);

  useEffect(() => {
    const record = models.get(path);
    if (record && record.model.getValue() !== content) {
      record.synchronizing = true;
      try {
        record.model.setValue(content);
      } finally {
        record.synchronizing = false;
      }
    }
  }, [content, models, path]);

  useEffect(() => {
    const codeEditor = instance.current;
    if (jump && codeEditor) {
      codeEditor.setPosition({ lineNumber: jump.line, column: 1 });
      codeEditor.revealLineInCenter(jump.line);
    }
  }, [jump]);
  return (
    <div className="code-editor">
      <div ref={mount} className="code-editor-target" />
      {!ready && <p role={error ? "alert" : "status"} className="file-help">{error || "Loading source editor…"}</p>}
    </div>
  );
}
