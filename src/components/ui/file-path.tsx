import { Copy, MoreHorizontal } from "lucide-react";
import { DropdownMenu } from "radix-ui";
import { useState } from "react";
import { ControlTooltip } from "@/components/ui/control-tooltip";

export function PathText({ path }: { path: string }) {
  const separator = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return <span className="ui-file-path">
    {separator >= 0 && <span className="ui-file-parent">{path.slice(0, separator + 1)}</span>}
    <span className="ui-file-name">{path.slice(separator + 1)}</span>
  </span>;
}

export function CopyFilePath({ path }: { path: string }) {
  const [copied, setCopied] = useState<{ path: string; ok: boolean }>();
  async function copy() {
    try {
      await navigator.clipboard.writeText(path);
      setCopied({ path, ok: true });
    } catch {
      setCopied({ path, ok: false });
    }
  }
  return <div className="ui-file-path-actions">
    <DropdownMenu.Root>
      <ControlTooltip label={path}><DropdownMenu.Trigger className="ui-icon-button" aria-label="File path actions">
        <MoreHorizontal size={16} aria-hidden="true" />
      </DropdownMenu.Trigger></ControlTooltip>
      <DropdownMenu.Portal>
        <DropdownMenu.Content className="ui-action-menu" align="end" sideOffset={4} collisionPadding={8}>
          <DropdownMenu.Label className="ui-action-menu-label">{path}</DropdownMenu.Label>
          <DropdownMenu.Item onSelect={() => void copy()}><Copy size={14} aria-hidden="true" /> Copy path</DropdownMenu.Item>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
    {copied?.path === path && <span role={copied.ok ? "status" : "alert"}>{copied.ok ? "Copied" : "Could not copy path"}</span>}
  </div>;
}
