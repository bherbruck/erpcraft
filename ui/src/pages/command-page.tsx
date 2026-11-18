// Screens that run one command, at /c/:company/run/:command. A command whose
// input a generic form cannot take well (a file to read, for one) has a page of
// its own here; the registry decides whether the user may run it.

import type { ReactNode } from "react";
import { useParams } from "react-router";
import { useApp } from "@/app/context";
import { canRun, findCommand } from "@/lib/meta";
import { BANK_IMPORT, BankImportPage } from "./bank-import";
import { EDI_RECEIVE, EdiReceivePage } from "./edi";
import { EINVOICE_IMPORT, EinvoiceImportPage } from "./einvoice-import";
import { NotFoundPage } from "./not-found";

const PAGES: Record<string, () => ReactNode> = {
  [BANK_IMPORT]: BankImportPage,
  [EINVOICE_IMPORT]: EinvoiceImportPage,
  [EDI_RECEIVE]: EdiReceivePage,
};

/** Whether a command has a screen of its own. */
export function hasCommandPage(name: string): boolean {
  return name in PAGES;
}

export function CommandPage() {
  const { meta } = useApp();
  const params = useParams();
  const command = findCommand(meta, params.command);
  const Page = command ? PAGES[command.name] : undefined;
  if (!command || !Page || !canRun(meta, command.name)) return <NotFoundPage />;
  return <Page />;
}
