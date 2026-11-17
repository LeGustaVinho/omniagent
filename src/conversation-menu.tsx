import { Activity, Archive, ChevronRight, Clipboard, Columns2, Download, Ellipsis, Eye, GitFork, Link, ListChecks, ListPlus, MessageSquare, Pencil, Pin, Trash2 } from "lucide-react";
import { ContextMenu, DropdownMenu } from "radix-ui";
import { useRef, useState, type ReactNode } from "react";

export type ConversationAction =
  | "open" | "open-beside" | "activity" | "rename" | "pin" | "unread" | "move" | "new-section" | "fork"
  | "copy-title" | "copy-id" | "copy-worktree" | "copy-reference" | "export-markdown" | "archive" | "delete" | "select-to-move";

export type ConversationMenuProps = {
  title: string;
  pinned?: boolean;
  unread?: boolean;
  archived?: boolean;
  sections?: readonly { id: string; name: string }[];
  onAction: (action: ConversationAction, section?: string) => void;
  available?: (action: ConversationAction) => boolean;
};

type ItemDescription = {
  action: ConversationAction;
  label: string;
  icon?: ReactNode;
  section?: string;
};

function readActions(props: ConversationMenuProps) {
  const navigation: ItemDescription[] = [
    { action: "open", label: "Open conversation", icon: <MessageSquare /> },
    { action: "open-beside", label: "Open conversation to the side", icon: <Columns2 /> },
    { action: "activity", label: "Inspect latest attempt", icon: <Activity /> },
  ];
  const organize: ItemDescription[] = [
    { action: "rename", label: "Rename", icon: <Pencil /> },
    { action: "pin", label: props.pinned ? "Unpin" : "Pin", icon: <Pin /> },
    { action: "unread", label: props.unread ? "Mark as read" : "Mark as unread", icon: <Eye /> },
    { action: "select-to-move", label: "Select conversations to move…", icon: <ListChecks /> },
  ];
  const section: ItemDescription[] = [
    { action: "move", label: "No section" },
    ...(props.sections ?? []).map(({ id, name }) => ({ action: "move" as const, label: name, section: id })),
    { action: "new-section", label: "New section…", icon: <ListPlus /> },
  ];
  const fork: ItemDescription = { action: "fork", label: "Create fork…", icon: <GitFork /> };
  const exportTranscript: ItemDescription = { action: "export-markdown", label: "Export conversation as Markdown…", icon: <Download /> };
  const copy: ItemDescription[] = [
    { action: "copy-title", label: "Title" },
    { action: "copy-id", label: "Conversation ID" },
    { action: "copy-worktree", label: "Worktree path" },
    { action: "copy-reference", label: "Local reference", icon: <Link /> },
  ];
  const retain: ItemDescription[] = [
    { action: "archive", label: props.archived ? "Unarchive" : "Archive", icon: <Archive /> },
    { action: "delete", label: "Delete permanently…", icon: <Trash2 /> },
  ];
  return { navigation, organize, section, fork, copy, exportTranscript, retain };
}

function ConversationMenuContent({ variant, ...props }: ConversationMenuProps & { variant: "context" | "dropdown" }) {
  const { Item, Separator, Sub, SubTrigger, SubContent, Portal } = variant === "context" ? ContextMenu : DropdownMenu;
  const { navigation, organize, section, fork, copy, exportTranscript, retain } = readActions(props);
  const availableItems = (items: ItemDescription[]) => items.filter(({ action }) => props.available?.(action) ?? !["open-beside", "select-to-move"].includes(action));
  const retentionItems = availableItems(retain);
  const navigationItems = availableItems(navigation);

  function item({ action, label, icon, section }: ItemDescription) {
    return (
      <Item key={`${action}:${section ?? ""}`} className="conv-menu-item"
        data-destructive={action === "delete" ? "" : undefined}
        onSelect={() => props.onAction(action, section)}>
        {icon}<span>{label}</span>
      </Item>
    );
  }

  function submenu(label: string, icon: ReactNode, actions: ItemDescription[]) {
    const items = availableItems(actions);
    if (!items.length) return null;
    return (
      <Sub>
        <SubTrigger className="conv-menu-item">
          {icon}<span>{label}</span><ChevronRight className="conv-menu-chevron" />
        </SubTrigger>
        <Portal><SubContent className="conv-menu" collisionPadding={8}>{items.map(item)}</SubContent></Portal>
      </Sub>
    );
  }

  return (
    <>
      {navigationItems.map(item)}
      {navigationItems.length > 0 && <Separator className="conv-menu-separator" />}
      {availableItems(organize).map(item)}
      {submenu("Section", <ListPlus />, section)}
      <Separator className="conv-menu-separator" />
      {availableItems([fork]).map(item)}
      {submenu("Copy", <Clipboard />, copy)}
      {availableItems([exportTranscript]).map(item)}
      {retentionItems.length > 0 && <Separator className="conv-menu-separator" />}
      {retentionItems.map(item)}
    </>
  );
}

export function ConversationRow({ children, ...props }: ConversationMenuProps & {
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const keyboardTarget = useRef<HTMLElement>(null);
  return (
    <ContextMenu.Root>
      <ContextMenu.Trigger asChild>
        <div className="conv-session-row" onKeyDown={(event) => {
          if (!(event.key === "ContextMenu" || (event.shiftKey && event.key === "F10"))) return;
          event.preventDefault();
          event.stopPropagation();
          keyboardTarget.current = event.target as HTMLElement;
          setOpen(true);
        }}>
          {children}
          <DropdownMenu.Root open={open} onOpenChange={setOpen}>
            <DropdownMenu.Trigger className="conv-row-more" aria-label={`Actions for ${props.title}`} title={`Actions for ${props.title}`}>
              <Ellipsis aria-hidden />
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              <DropdownMenu.Content className="conv-menu" align="end" sideOffset={5} collisionPadding={8} onCloseAutoFocus={(event) => {
                const target = keyboardTarget.current;
                keyboardTarget.current = null;
                if (!target?.isConnected || event.defaultPrevented) return;
                event.preventDefault();
                target.focus();
              }}>
                <ConversationMenuContent variant="dropdown" {...props} />
              </DropdownMenu.Content>
            </DropdownMenu.Portal>
          </DropdownMenu.Root>
        </div>
      </ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Content className="conv-menu" collisionPadding={8}>
          <ConversationMenuContent variant="context" {...props} />
        </ContextMenu.Content>
      </ContextMenu.Portal>
    </ContextMenu.Root>
  );
}
