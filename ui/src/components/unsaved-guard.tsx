import { useCallback, useEffect, useRef } from "react";
import { type Blocker, useBlocker } from "react-router";
import { Button } from "@/components/button";
import { Dialog } from "@/components/dialog";
import { t } from "@/i18n/t";

/**
 * Asks before the user leaves a form with unsaved changes, whether by a link,
 * the Back button, or closing the tab.
 *
 * `allowNext()` lets one navigation through: the one the form itself makes
 * after a successful save.
 */
export function useUnsavedGuard(dirty: boolean): { blocker: Blocker; allowNext: () => void } {
  const allowed = useRef(false);
  const blocker = useBlocker(({ currentLocation, nextLocation }) => {
    if (allowed.current) {
      allowed.current = false;
      return false;
    }
    // Changing the tab or a filter stays on the form and loses nothing.
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

export function UnsavedDialog({ blocker }: { blocker: Blocker }) {
  const blocked = blocker.state === "blocked";
  return (
    <Dialog
      open={blocked}
      onClose={() => blocker.reset?.()}
      title={t("unsaved.title")}
      description={t("unsaved.description")}
      footer={
        <>
          <Button variant="destructive" onClick={() => blocker.proceed?.()}>
            {t("unsaved.leave")}
          </Button>
          <Button autoFocus onClick={() => blocker.reset?.()}>
            {t("unsaved.stay")}
          </Button>
        </>
      }
    />
  );
}
