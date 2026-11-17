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
      followAppTheme(monaco);
      return monaco;
    }).catch((error: unknown) => {
      pending = undefined;
      throw error;
    });
  }
  return pending;
}

function followAppTheme(monaco: Monaco) {
  const root = document.documentElement;
  const forcedColors = window.matchMedia("(forced-colors: active)");
  let definedTheme: string | undefined;
  const update = () => {
    const light = root.dataset.appTheme === "light";
    const theme = light ? "omniagent-light" : "omniagent-dark";
    if (definedTheme !== theme) {
      defineAppTheme(monaco, theme, light ? "vs" : "vs-dark", getComputedStyle(root));
      definedTheme = theme;
    }
    monaco.editor.setTheme(forcedColors.matches ? (light ? "hc-light" : "hc-black") : theme);
  };
  update();
  new MutationObserver(update).observe(root, { attributes: true, attributeFilter: ["data-app-theme"] });
  forcedColors.addEventListener("change", update);
}

function defineAppTheme(monaco: Monaco, theme: string, base: "vs" | "vs-dark", style: CSSStyleDeclaration) {
  const color = (name: string) => style.getPropertyValue(name).trim();
  const background = color("--background");
  const foreground = color("--foreground");
  const muted = color("--muted");
  const mutedForeground = color("--muted-foreground");
  const elevated = color("--elevated");
  const border = color("--border");
  const primary = color("--primary");
  const success = color("--success");
  const warning = color("--warning");
  const danger = color("--danger");
  const keyword = color("--syntax-keyword");
  const stringLiteral = color("--syntax-string");
  const typeName = color("--syntax-type");
  const numericLiteral = color("--syntax-number");

  monaco.editor.defineTheme(theme, {
    base,
    inherit: true,
    rules: [
      { token: "comment", foreground: mutedForeground },
      { token: "keyword", foreground: keyword },
      { token: "keyword.flow", foreground: keyword },
      { token: "keyword.json", foreground: numericLiteral },
      { token: "string", foreground: stringLiteral },
      { token: "string.key.json", foreground: typeName },
      { token: "string.value.json", foreground: stringLiteral },
      { token: "string.yaml", foreground: stringLiteral },
      { token: "string.html", foreground: stringLiteral },
      { token: "string.sql", foreground: stringLiteral },
      { token: "number", foreground: numericLiteral },
      { token: "number.hex", foreground: numericLiteral },
      { token: "constant", foreground: numericLiteral },
      { token: "regexp", foreground: stringLiteral },
      { token: "type", foreground: typeName },
      { token: "tag", foreground: typeName },
      { token: "attribute.name", foreground: numericLiteral },
      { token: "attribute.value", foreground: stringLiteral },
    ],
    colors: {
      "foreground": foreground,
      "focusBorder": color("--ring"),
      "editor.background": background,
      "editor.foreground": foreground,
      "editorGutter.background": background,
      "editorLineNumber.foreground": mutedForeground,
      "editorLineNumber.activeForeground": foreground,
      "editorIndentGuide.background1": border,
      "editorIndentGuide.activeBackground1": color("--input"),
      "editorCursor.foreground": primary,
      "editor.lineHighlightBackground": `${muted}A6`,
      "editor.lineHighlightBorder": "#00000000",
      "editor.selectionBackground": `${primary}47`,
      "editor.inactiveSelectionBackground": `${primary}2E`,
      "editor.selectionHighlightBackground": `${primary}24`,
      "editor.findMatchBackground": `${warning}59`,
      "editor.findMatchHighlightBackground": `${warning}33`,
      "editorBracketHighlight.foreground1": primary,
      "editorBracketHighlight.foreground2": numericLiteral,
      "editorBracketHighlight.foreground3": success,
      "editorBracketHighlight.unexpectedBracket.foreground": danger,
      "editorError.foreground": danger,
      "editorWarning.foreground": warning,
      "editorWidget.background": elevated,
      "editorWidget.foreground": foreground,
      "editorWidget.border": border,
      "input.background": background,
      "input.border": color("--input"),
      "input.foreground": foreground,
      "list.activeSelectionBackground": muted,
      "list.activeSelectionForeground": foreground,
      "list.highlightForeground": primary,
      "list.hoverBackground": muted,
      "menu.background": elevated,
      "menu.border": border,
      "menu.selectionBackground": muted,
      "menu.selectionForeground": foreground,
      "diffEditor.border": border,
      "diffEditor.insertedTextBackground": `${success}38`,
      "diffEditor.removedTextBackground": `${danger}38`,
      "diffEditor.insertedLineBackground": `${success}1A`,
      "diffEditor.removedLineBackground": `${danger}1A`,
      "diffEditorGutter.insertedLineBackground": `${success}38`,
      "diffEditorGutter.removedLineBackground": `${danger}38`,
    },
  });
}
