// A printed document is HTML the server renders from a template. The server
// refuses active content in templates and checks the rendered page again; this
// is the second wall: the page is shown in an iframe sandboxed without scripts
// and without its own origin, and printed from a window that forbids scripts.

import { t } from "@/i18n/t";
import { cn } from "@/lib/utils";

/** The policy the printed page runs under, whatever it contains: no script of any kind, nothing fetched. */
const PRINT_POLICY = "default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:";

function withPolicy(html: string): string {
  const meta = `<meta http-equiv="Content-Security-Policy" content="${PRINT_POLICY}">`;
  const head = /<head[^>]*>/i.exec(html);
  if (head) return `${html.slice(0, head.index + head[0].length)}${meta}${html.slice(head.index + head[0].length)}`;
  return `<!doctype html><html><head>${meta}</head><body>${html}</body></html>`;
}

/**
 * Prints a rendered page. A sandboxed frame without scripts cannot be told to
 * print, so the page opens in a window of its own, from a Blob URL, with a
 * policy that forbids every script (and the app's own policy, which a Blob
 * document inherits). The app asks that window to print, and it closes after.
 * Returns false when the browser did not open the window (a pop-up blocker).
 */
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
export function PrintFrame({ html, title, className }: { html: string; title: string; className?: string }) {
  return (
    <iframe
      title={t("print.frame", { title })}
      // An empty sandbox: no scripts, no forms, no pop-ups, no origin of its own.
      sandbox=""
      srcDoc={withPolicy(html)}
      className={cn("w-full rounded-md border bg-paper", className)}
    />
  );
}
