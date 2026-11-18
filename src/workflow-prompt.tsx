import type { ReactNode } from "react";
import { MessageSquareText } from "lucide-react";
import "./workflow-prompt.css";

export function PromptRevisionView({ children }: { children: ReactNode }) {
  return <section className="workflow-prompt-revision-view" aria-label="Reusable Prompt">
    <div className="workflow-inspector-heading"><span className="workflow-inspector-icon"><MessageSquareText size={17} aria-hidden /></span>
      <div><h2>Reusable Prompt</h2></div>
    </div>
    {children}
  </section>;
}
