import type { ReactNode } from "react";
import type { ApiError } from "@/api/client";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Dialog } from "@/components/ui/dialog";
import { t } from "@/i18n/t";

/** Asks before something that cannot be taken back. The safe button has the focus. */
export function ConfirmDialog({
  open,
  title,
  description,
  confirmLabel,
  busy = false,
  error = null,
  onConfirm,
  onClose,
}: {
  open: boolean;
  title: ReactNode;
  description: ReactNode;
  confirmLabel: string;
  busy?: boolean;
  error?: ApiError | null;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <Dialog
      open={open}
      onClose={onClose}
      dirty={busy}
      title={title}
      description={description}
      footer={
        <>
          <Button variant="outline" autoFocus onClick={onClose} disabled={busy}>
            {t("action.cancel")}
          </Button>
          <Button variant="destructive" busy={busy} onClick={onConfirm}>
            {confirmLabel}
          </Button>
        </>
      }
    >
      {error ? <Alert>{error.message}</Alert> : null}
    </Dialog>
  );
}
