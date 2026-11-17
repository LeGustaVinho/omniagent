import { MessageSquare, PanelLeftClose, Plus, Search } from "lucide-react";
import { Dialog } from "radix-ui";
import type { KeyboardEventHandler, ReactNode, Ref, UIEventHandler } from "react";
import type { MessageRole } from "@/bindings/MessageRole";
import { ResizablePanel } from "@/panel-resize";
import { ControlTooltip } from "@/components/ui/control-tooltip";
import "./conversation.css";

export function ConversationWorkspaceLayout({ navigation, controls, children }: {
  navigation: ReactNode;
  controls?: ReactNode;
  children: ReactNode;
}) {
  return <div className="conv-workspace">
    {controls}
    <div className="conv-workspace-body">
      {navigation}
      <div className="conv-workspace-main">{children}</div>
    </div>
  </div>;
}

export function ConversationNavigation({ compact, open, onOpenChange, children }: {
  compact: boolean;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  children: ReactNode;
}) {
  if (!compact) return open ? <ResizablePanel as="aside" className="conv-navigation" aria-label="Conversation navigation"
    edge="right" resizeKey="conversation-navigation" resizeLabel="conversation navigation">{children}</ResizablePanel> : null;
  return <Dialog.Root open={open} onOpenChange={onOpenChange}>
    <Dialog.Portal>
      <Dialog.Overlay className="work-shade" />
      <Dialog.Content asChild aria-describedby={undefined} onOpenAutoFocus={(event) => {
        const search = (event.currentTarget as HTMLElement | null)?.querySelector<HTMLInputElement>('input[type="search"]');
        if (search) {
          event.preventDefault();
          search.focus();
        }
      }}>
        <ResizablePanel className="conv-navigation conv-navigation-drawer" edge="right" resizeKey="conversation-navigation" resizeLabel="conversation navigation">
          <Dialog.Title className="sr-only">Conversation navigation</Dialog.Title>
          <Dialog.Close className="conv-icon-button conv-navigation-close" aria-label="Close conversation navigation">
            <PanelLeftClose aria-hidden />
          </Dialog.Close>
          {children}
        </ResizablePanel>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}

export function ConversationSidePanel({ label, onKeyDown, children }: {
  label: string; onKeyDown?: KeyboardEventHandler<HTMLElement>; children: ReactNode;
}) {
  return <ResizablePanel as="aside" className="conv-activity" aria-label={label} edge="left"
    resizeKey="conversation-side-panel" resizeLabel={label} onKeyDown={onKeyDown}>{children}</ResizablePanel>;
}

export function ConversationLibraryLayout({ sectionRef, className, query, searchLabel, onQuery, onCreate, creating, searchAction, children }: {
  sectionRef?: Ref<HTMLElement>;
  className?: string;
  query: string;
  searchLabel: string;
  onQuery: (query: string) => void;
  onCreate: () => void;
  creating?: boolean;
  searchAction?: ReactNode;
  children: ReactNode;
}) {
  return <section ref={sectionRef} className={`conv-sessions conv-library-frame${className ? ` ${className}` : ""}`} aria-label="Conversation library">
    <header className="conv-session-heading">
      <h2>Conversations</h2>
      <ControlTooltip label="New conversation"><button type="button" className="conv-icon-button conv-library-create"
        aria-label="New conversation" disabled={creating} onClick={onCreate}>
        <Plus aria-hidden /><span>{creating ? "Creating…" : "New"}</span>
      </button></ControlTooltip>
    </header>
    <div className="flex items-center gap-1">
      <label className="conv-search min-w-0 flex-1"><Search aria-hidden />
        <input type="search" aria-label={searchLabel} placeholder={searchLabel} value={query}
          onChange={(event) => onQuery(event.currentTarget.value)} />
      </label>
      {searchAction && <div className="shrink-0">{searchAction}</div>}
    </div>
    <div className="conv-library-entries">{children}</div>
  </section>;
}

export function ConversationLibraryGroup({ title, children }: { title: string; children: ReactNode }) {
  return <section aria-label={title}>
    <h3 className="conv-session-group">{title}</h3>
    <ul className="conv-session-list">{children}</ul>
  </section>;
}

export function ConversationTitleBar({ title, editor, rename, details, children }: {
  title: string;
  editor?: ReactNode;
  rename?: ReactNode;
  details?: ReactNode;
  children: ReactNode;
}) {
  return <header className="conv-heading">
    <div className="conv-heading-title">
      <span className="conv-heading-icon"><MessageSquare aria-hidden /></span>
      {editor ?? <><h1 title={title}>{title}</h1>{rename}{details}</>}
    </div>
    <div className="conv-heading-actions">{children}</div>
  </header>;
}

export function ConversationTranscript({ historyRef, contentRef, onScroll, children }: {
  historyRef: Ref<HTMLDivElement>;
  contentRef?: Ref<HTMLDivElement>;
  onScroll: UIEventHandler<HTMLDivElement>;
  children: ReactNode;
}) {
  return <div ref={historyRef} className="conv-history" role="region" aria-label="Conversation history" tabIndex={0} onScroll={onScroll}>
    <div ref={contentRef} className="conv-reading">{children}</div>
  </div>;
}

export function ConversationMessage({ role, actions, children }: {
  role: MessageRole;
  actions: ReactNode;
  children: ReactNode;
}) {
  return <li className={role === "user" ? "conv-user" : "conv-assistant"}>
    <p className="sr-only">{role === "user" ? "You" : "Assistant"}</p>
    {children}
    <div className="conv-message-actions">{actions}</div>
  </li>;
}

export function ConversationComposerToolbar({ actions, children }: { actions: ReactNode; children: ReactNode }) {
  return <div className="conv-composer-toolbar">
    <div className="conv-composer-options">{children}</div>
    <div className="conv-composer-actions">{actions}</div>
  </div>;
}

export function ConversationEmptyState({ title, description, children }: {
  title: string;
  description?: string;
  children?: ReactNode;
}) {
  return <div className="conv-empty">
    <span className="conv-empty-icon"><MessageSquare aria-hidden /></span>
    <h2>{title}</h2>
    {description && <p>{description}</p>}
    {children && <div className="conv-empty-actions">{children}</div>}
  </div>;
}
