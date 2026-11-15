import { Archive, ChevronRight, Clipboard, Ellipsis, Eye, GitFork, Link, ListPlus, Pencil, Pin, Trash2 } from "lucide-react";
import { ContextMenu, DropdownMenu } from "radix-ui";
import { useState, type ReactNode } from "react";

export type ConversationAction =
  | "rename" | "pin" | "unread" | "move" | "new-section" | "fork"
  | "copy-title" | "copy-reference" | "archive" | "delete";

export type ConversationMenuProps = {
  title: string;
  pinned?: boolean;
  unread?: boolean;
  archived?: boolean;
  sections?: readonly string[];
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
  const organize: ItemDescription[] = [
    { action: "rename", label: "Rename", icon: <Pencil /> },
    { action: "pin", label: props.pinned ? "Unpin" : "Pin", icon: <Pin /> },
    { action: "unread", label: props.unread ? "Mark as read" : "Mark as unread", icon: <Eye /> },
  ];
  const section: ItemDescription[] = [
    { action: "move", label: "No section" },
    ...(props.sections ?? []).map((section) => ({ action: "move" as const, label: section, section })),
    { action: "new-section", label: "New section…", icon: <ListPlus /> },
  ];
  const fork: ItemDescription = { action: "fork", label: "Create fork…", icon: <GitFork /> };
  const copy: ItemDescription[] = [
    { action: "copy-title", label: "Title" },
    { action: "copy-reference", label: "Local reference", icon: <Link /> },
  ];
  const retain: ItemDescription[] = [
    { action: "archive", label: props.archived ? "Unarchive" : "Archive", icon: <Archive /> },
    { action: "delete", label: "Delete permanently…", icon: <Trash2 /> },
  ];
  return { organize, section, fork, copy, retain };
}

function ConversationMenuContent({ variant, ...props }: ConversationMenuProps & { variant: "context" | "dropdown" }) {
  const { Item, Separator, Sub, SubTrigger, SubContent, Portal } = variant === "context" ? ContextMenu : DropdownMenu;
  const { organize, section, fork, copy, retain } = readActions(props);
  const available = (action: ConversationAction) => props.available?.(action) ?? true;

  function item({ action, label, icon, section }: ItemDescription) {
    return (
      <Item key={`${action}:${section ?? ""}`} className="conv-menu-item" disabled={!available(action)}
        title={available(action) ? undefined : "Integration pending"}
        data-destructive={action === "delete" ? "" : undefined}
        onSelect={() => props.onAction(action, section)}>
        {icon}<span>{label}</span>
      </Item>
    );
  }

  function submenu(label: string, icon: ReactNode, actions: ItemDescription[]) {
    return (
      <Sub>
        <SubTrigger className="conv-menu-item" disabled={!actions.some(({ action }) => available(action))}>
          {icon}<span>{label}</span><ChevronRight className="conv-menu-chevron" />
        </SubTrigger>
        <Portal><SubContent className="conv-menu" collisionPadding={8}>{actions.map(item)}</SubContent></Portal>
      </Sub>
    );
  }

  return (
    <>
      {organize.map(item)}
      {submenu("Section", <ListPlus />, section)}
      <Separator className="conv-menu-separator" />
      {item(fork)}
      {submenu("Copy", <Clipboard />, copy)}
      <Separator className="conv-menu-separator" />
      {retain.map(item)}
      {props.available && <p className="conv-menu-note">Unavailable actions · Integration pending</p>}
    </>
  );
}

export function ConversationRow({ children, ...props }: ConversationMenuProps & {
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  return (
    <ContextMenu.Root>
      <ContextMenu.Trigger asChild>
        <div className="conv-session-row" onKeyDown={(event) => {
          if (!(event.key === "ContextMenu" || (event.shiftKey && event.key === "F10"))) return;
          event.preventDefault();
          setOpen(true);
        }}>
          {children}
          <DropdownMenu.Root open={open} onOpenChange={setOpen}>
            <DropdownMenu.Trigger className="conv-row-more" aria-label={`Actions for ${props.title}`} title={`Actions for ${props.title}`}>
              <Ellipsis aria-hidden />
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              <DropdownMenu.Content className="conv-menu" align="end" sideOffset={5} collisionPadding={8}>
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
