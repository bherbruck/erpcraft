import { AlertDialog } from "@astryxdesign/core/AlertDialog";
import { useCallback, useEffect, useRef } from "react";
import { type Blocker, useBlocker } from "react-router";
import { t } from "@/i18n/t";

/**
 * Asks before the user leaves a form with unsaved changes, by a link, Back, or
 * closing the tab. `allowNext()` lets the form's own navigation after a save through.
 */
export function useUnsavedGuard(dirty: boolean): { blocker: Blocker; allowNext: () => void } {
  const allowed = useRef(false);
  const blocker = useBlocker(({ currentLocation, nextLocation }) => {
    if (allowed.current) {
      allowed.current = false;
      return false;
    }
    return dirty && currentLocation.pathname !== nextLocation.pathname;
  });
  useEffect(() => {
    if (!dirty) return;
    const warn = (event: BeforeUnloadEvent): void => event.preventDefault();
    window.addEventListener("beforeunload", warn);
    return () => window.removeEventListener("beforeunload", warn);
  }, [dirty]);
  const allowNext = useCallback(() => {
    allowed.current = true;
  }, []);
  return { blocker, allowNext };
}

/** Astryx's AlertDialog as it comes: the destructive choice is not the default. */
export function UnsavedDialog({ blocker }: { blocker: Blocker }) {
  return (
    <AlertDialog
      isOpen={blocker.state === "blocked"}
      onOpenChange={(open) => (open ? null : blocker.reset?.())}
      title={t("unsaved.title")}
      description={t("unsaved.description")}
      actionLabel={t("unsaved.leave")}
      cancelLabel={t("unsaved.stay")}
      onAction={() => blocker.proceed?.()}
    />
  );
}
