import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useState } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import type { ConversationCode } from "@/conversation-markdown";
import { FileDialog } from "@/ide-file-workspace";
import { useWorkspace } from "@/workspace-state";

const extensions: Record<string, string> = {
  powershell: "ps1", ps1: "ps1", pwsh: "ps1", bash: "sh", shell: "sh", sh: "sh",
  javascript: "js", js: "js", jsx: "jsx", typescript: "ts", ts: "ts", tsx: "tsx",
  python: "py", py: "py", rust: "rs", rs: "rs", json: "json", yaml: "yaml", yml: "yml",
  markdown: "md", md: "md", html: "html", xml: "xml", sql: "sql", css: "css", toml: "toml",
};

export function ConversationCodeFileDialog({ checkout, block, close, created }: {
  checkout: ProjectCheckout;
  block: ConversationCode;
  close: () => void;
  created: (path: string) => void;
}) {
  const checkoutId = checkout.checkoutId;
  const [name, setName] = useState(() => {
    const language = block.language?.toLowerCase() ?? "";
    return `code.${Object.hasOwn(extensions, language) ? extensions[language] : "txt"}`;
  });
  const [folder, setFolder] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const folders = useQuery({
    queryKey: ["checkout-folders", checkoutId],
    queryFn: () => invoke<string[]>("list_checkout_folders", { checkoutId }),
    retry: false,
  });
  async function save() {
    if (busy || !name.trim()) return;
    if (useWorkspace.getState().checkout?.checkoutId !== checkoutId) {
      setError("The selected worktree changed. Reopen this code block in its worktree.");
      return;
    }
    setBusy(true);
    setError("");
    try {
      const path = await invoke<string>("create_checkout_item", {
        checkoutId, relativeDirectory: folder, name, directory: false, contents: block.text,
      });
      close();
      created(path);
    } catch (failure) {
      setError(String(failure));
    } finally {
      setBusy(false);
    }
  }
  return <FileDialog title="Save code block as file" description={checkout.checkoutRoot} onClose={() => { if (!busy) close(); }}>
    <form className="grid gap-2" onSubmit={(event) => { event.preventDefault(); void save(); }}>
      <label className="grid gap-1 text-xs">Destination folder
        <select className="delivery-filter" value={folder} disabled={busy} onChange={(event) => setFolder(event.currentTarget.value)}>
          <option value="">Checkout root</option>
          {folders.data?.map((path) => <option key={path} value={path}>{path}</option>)}
        </select>
      </label>
      {folders.error && <p className="file-help" role="alert">{String(folders.error)}</p>}
      <label className="file-field">File name<input autoFocus value={name} disabled={busy} onChange={(event) => setName(event.currentTarget.value)} /></label>
      <pre className="max-h-48 overflow-auto whitespace-pre-wrap wrap-anywhere font-mono text-xs" tabIndex={0}
        aria-label="Code to save">{block.text}</pre>
      {error && <p className="file-help" role="alert">{error}</p>}
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" disabled={busy} onClick={close}>Cancel</button>
        <button type="submit" className="fw-btn fw-primary" disabled={busy || !name.trim()}>{busy ? "Saving…" : "Save new file"}</button>
      </div>
    </form>
  </FileDialog>;
}
