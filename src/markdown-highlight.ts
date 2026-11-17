import powershell from "highlight.js/lib/languages/powershell";
import { common } from "lowlight";
import rehypeHighlight, { type Options } from "rehype-highlight";
import "./markdown-highlight.css";

export const markdownHighlight: [typeof rehypeHighlight, Options] = [rehypeHighlight, {
  languages: { ...common, powershell },
  plainText: ["mermaid"],
}];
