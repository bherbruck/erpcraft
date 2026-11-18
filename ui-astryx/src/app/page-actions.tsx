// What the page on screen can do, offered in the command palette as well as on
// the page. A page registers its actions while it shows a record; the palette
// lists them and runs one exactly as the page's own button would (a follow-on
// draft at once, anything else through its preview).

import { createContext, type ReactNode, useContext, useEffect, useState } from "react";

export interface PageAction {
  id: string;
  label: string;
  destructive: boolean;
  run: () => void;
}

export interface PageActions {
  /** What the actions are about: "Purchase order PO-2026-00001". */
  subject: string;
  actions: PageAction[];
}

const Context = createContext<{ current: PageActions | null; set: (actions: PageActions | null) => void } | null>(null);

export function PageActionsProvider({ children }: { children: ReactNode }) {
  const [current, set] = useState<PageActions | null>(null);
  return <Context.Provider value={{ current, set }}>{children}</Context.Provider>;
}

/** The actions of the page on screen, for the palette. */
export function useCurrentPageActions(): PageActions | null {
  return useContext(Context)?.current ?? null;
}

/** Offers a page's actions in the palette while the page is shown. */
export function useOfferPageActions(actions: PageActions | null): void {
  const set = useContext(Context)?.set;
  const key = actions ? `${actions.subject}|${actions.actions.map((action) => action.id).join(",")}` : "";
  // biome-ignore lint/correctness/useExhaustiveDependencies: the key says when the offer changed; the callbacks follow it
  useEffect(() => {
    set?.(actions);
    return () => set?.(null);
  }, [key, set]);
}
