import { Dialog } from "radix-ui";
import { useState } from "react";

export type LibraryMoveTarget = {
  checkoutId: string;
  conversationId: string;
  expectedSectionId: string | null;
};

export type LibraryMoveItem = LibraryMoveTarget & { title: string; worktree: string };

export function LibraryBatchMoveDialog({ items, sections, busy, error, restoreFocus, onClose, onMove }: {
  items: readonly LibraryMoveItem[];
  sections: readonly { id: string; name: string }[];
  busy: boolean;
  error: Error | null;
  restoreFocus: (event: Event) => void;
  onClose: () => void;
  onMove: (sectionId: string | null) => void;
}) {
  const [destination, setDestination] = useState<string | null>();
  const canMove = destination !== undefined && !busy && items.some(({ expectedSectionId }) => expectedSectionId !== destination);
  return <Dialog.Root open onOpenChange={(shown) => { if (!shown && !busy) onClose(); }}>
    <Dialog.Portal>
      <Dialog.Overlay className="conv-dialog-scrim" />
      <Dialog.Content className="conv-dialog" onCloseAutoFocus={restoreFocus}>
        <Dialog.Title>Move selected conversations</Dialog.Title>
        <Dialog.Description>{items.length} selected</Dialog.Description>
        <form className="conv-dialog-form" onSubmit={(event) => {
          event.preventDefault();
          if (canMove) onMove(destination);
        }}>
          <ul className="grid max-h-36 list-disc gap-1 overflow-auto pl-4 text-sm">
            {items.map(({ conversationId, title, worktree }) => <li key={conversationId} className="break-words">
              <span>{title}</span>
              <small className="block break-all text-muted-foreground">{worktree}</small>
            </li>)}
          </ul>
          <label>Destination section
            <select className="conv-select" value={destination === undefined ? "choose" : destination ?? ""} disabled={busy}
              onChange={({ currentTarget }) => setDestination(currentTarget.value || null)}>
              <option value="choose" disabled>Choose a section</option>
              <option value="">No section</option>
              {sections.map(({ id, name }) => <option key={id} value={id}>{name}</option>)}
            </select>
          </label>
          {error && <p role="alert" className="text-destructive">{String(error)}</p>}
          <footer>
            <Dialog.Close asChild><button type="button" className="conv-quiet-button" disabled={busy}>Cancel</button></Dialog.Close>
            <button type="submit" className="conv-primary-button" disabled={!canMove}>{busy ? "Moving…" : "Move"}</button>
          </footer>
        </form>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
