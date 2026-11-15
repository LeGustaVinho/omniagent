import EditorWorker from "monaco-editor/editor/editor.worker.js?worker";
import CssWorker from "monaco-editor/language/css/css.worker.js?worker";
import HtmlWorker from "monaco-editor/language/html/html.worker.js?worker";
import JsonWorker from "monaco-editor/language/json/json.worker.js?worker";
import TypeScriptWorker from "monaco-editor/language/typescript/ts.worker.js?worker";

type Monaco = typeof import("monaco-editor");
type WorkerHost = typeof globalThis & { MonacoEnvironment?: { getWorker: (id: string, label: string) => Worker } };

let pending: Promise<Monaco> | undefined;

export function loadMonaco(): Promise<Monaco> {
  if (!pending) {
    (globalThis as WorkerHost).MonacoEnvironment = {
      getWorker(_id, label) {
        switch (label) {
          case "json": return new JsonWorker();
          case "css":
          case "scss":
          case "less": return new CssWorker();
          case "html":
          case "handlebars":
          case "razor": return new HtmlWorker();
          case "typescript":
          case "javascript": return new TypeScriptWorker();
          default: return new EditorWorker();
        }
      },
    };
    pending = import("monaco-editor").then((monaco) => {
      monaco.editor.defineTheme("omniagent-dark", {
        base: "vs-dark",
        inherit: true,
        rules: [],
        colors: {
          "editor.background": "#191919",
          "diffEditor.insertedTextBackground": "#31975C55",
          "diffEditor.removedTextBackground": "#C6495355",
          "diffEditor.insertedLineBackground": "#22523564",
          "diffEditor.removedLineBackground": "#652C3464",
          "diffEditorGutter.insertedLineBackground": "#28563C",
          "diffEditorGutter.removedLineBackground": "#623139",
        },
      });
      return monaco;
    }).catch((error: unknown) => {
      pending = undefined;
      throw error;
    });
  }
  return pending;
}

export function monacoTheme(): string {
  const root = document.documentElement;
  const light = getComputedStyle(root).colorScheme === "light";
  return matchMedia("(forced-colors: active)").matches ? (light ? "hc-light" : "hc-black") : (light ? "vs" : "omniagent-dark");
}
