import * as DialogPrimitive from "@radix-ui/react-dialog";
import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

/**
 * A modal that closes only through its own buttons.
 *
 * Clicking outside does nothing. Escape closes it only while `dirty` is false,
 * that is, while nothing has been typed into it.
 */
export function Dialog({
  open,
  onClose,
  title,
  description,
  dirty = false,
  children,
  footer,
  className,
  wide = false,
}: {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  description?: ReactNode;
  dirty?: boolean;
  children?: ReactNode;
  /** The dialog's buttons. One of them must close it. */
  footer: ReactNode;
  className?: string;
  /** For content with tables, such as the ledger entries of a preview. */
  wide?: boolean;
}) {
  return (
    <DialogPrimitive.Root
      open={open}
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
    >
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-40 bg-foreground/30" />
        <DialogPrimitive.Content
          aria-describedby={description ? undefined : ""}
          onInteractOutside={(event) => event.preventDefault()}
          onPointerDownOutside={(event) => event.preventDefault()}
          onEscapeKeyDown={(event) => {
            if (dirty) event.preventDefault();
          }}
          // A dialog is a world of its own: keys typed in it are not shortcuts of the page underneath.
          onKeyDown={(event) => event.stopPropagation()}
          className={cn(
            wide ? "max-w-dialog-wide" : "max-w-dialog",
            "fixed start-1/2 top-[12vh] z-50 flex max-h-[76vh] w-[calc(100vw-2rem)] -translate-x-1/2 flex-col rounded-lg border bg-popover text-popover-foreground shadow-lg rtl:translate-x-1/2",
            className,
          )}
        >
          <div className="space-y-1 border-b px-4 py-3">
            <DialogPrimitive.Title className="text-base font-semibold">{title}</DialogPrimitive.Title>
            {description ? (
              <DialogPrimitive.Description className="text-sm text-muted-foreground">
                {description}
              </DialogPrimitive.Description>
            ) : null}
          </div>
          {children ? <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">{children}</div> : null}
          <div className="flex flex-wrap justify-end gap-2 border-t px-4 py-3">{footer}</div>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}
