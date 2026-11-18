// A printed document is HTML the server renders from a template. It is shown in an iframe
// sandboxed without scripts and without its own origin, and printed from a window that forbids
// scripts. (As ../ui; nothing here depends on the design system.)

import { t } from "@/i18n/t";

const PRINT_POLICY = "default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:";

function withPolicy(html: string): string {
  const meta = `<meta http-equiv="Content-Security-Policy" content="${PRINT_POLICY}">`;
  const head = /<head[^>]*>/i.exec(html);
  if (head) return `${html.slice(0, head.index + head[0].length)}${meta}${html.slice(head.index + head[0].length)}`;
  return `<!doctype html><html><head>${meta}</head><body>${html}</body></html>`;
}

/** Prints a rendered page from a Blob window that runs no script. False when a pop-up blocker stopped it. */
export function printHtml(html: string): boolean {
  const url = URL.createObjectURL(new Blob([withPolicy(html)], { type: "text/html" }));
  const win = window.open(url, "_blank");
  if (!win) {
    URL.revokeObjectURL(url);
    return false;
  }
  win.addEventListener("load", () => {
    win.focus();
    win.print();
    URL.revokeObjectURL(url);
  });
  win.addEventListener("afterprint", () => win.close());
  return true;
}

/** The rendered page, shown as it will print, in a frame that can run nothing. */
export function PrintFrame({ html, title }: { html: string; title: string }) {
  return <iframe title={t("print.frame", { title })} sandbox="" srcDoc={withPolicy(html)} className="print-frame" />;
}
