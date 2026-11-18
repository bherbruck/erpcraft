import type { ReactNode } from "react";
import {
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Dialog as ShadcnDialog,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

/**
 * A modal that closes only through its own buttons (GUIDELINES.md, Navigation 3),
 * built from shadcn/ui's Dialog.
 *
 * Clicking outside does nothing. Escape closes it only while `dirty` is false,
 * that is, while nothing has been typed into it. shadcn's corner close button is
 * left out: the footer's buttons are the way out.
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
    <ShadcnDialog
      open={open}
      onOpenChange={(next) => {
        if (!next) onClose();
      }}
    >
      <DialogContent
        showCloseButton={false}
        aria-describedby={description ? undefined : ""}
        onInteractOutside={(event) => event.preventDefault()}
        onPointerDownOutside={(event) => event.preventDefault()}
        onEscapeKeyDown={(event) => {
          if (dirty) event.preventDefault();
        }}
        // A dialog is a world of its own: keys typed in it are not shortcuts of the page underneath.
        onKeyDown={(event) => event.stopPropagation()}
        className={cn(
          wide ? "sm:max-w-5xl" : "sm:max-w-xl",
          "top-[8vh] flex max-h-[84vh] translate-y-0 flex-col gap-0 p-0",
          className,
        )}
      >
        <DialogHeader className="border-b px-6 py-4">
          <DialogTitle className="text-base font-semibold">{title}</DialogTitle>
          {description ? <DialogDescription>{description}</DialogDescription> : null}
        </DialogHeader>
        {children ? <div className="min-h-0 flex-1 overflow-y-auto px-6 py-4">{children}</div> : null}
        <DialogFooter className="border-t px-6 py-4">{footer}</DialogFooter>
      </DialogContent>
    </ShadcnDialog>
  );
}
