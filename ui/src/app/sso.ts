// The end of a single sign-on: the provider's sign-in comes back to the page
// the user started from, with a one-time `sso_code` in the address. The code is
// exchanged for a session (the server also checks a cookie it set when the
// sign-in started, so the request goes with this origin's cookies), and the
// code is removed from the address at once, before anything else reads it.

import { api } from "@/api/client";
import { paths } from "@/lib/paths";

const PARAM = "sso_code";

export async function finishSingleSignOn(): Promise<void> {
  const url = new URL(window.location.href);
  const code = url.searchParams.get(PARAM);
  if (!code) return;
  url.searchParams.delete(PARAM);
  window.history.replaceState(window.history.state, "", `${url.pathname}${url.search}${url.hash}`);
  try {
    await api.ssoExchange(code);
  } catch {
    // The code was used, expired or belongs to another browser.
    window.history.replaceState(null, "", `${paths.signIn()}?sso_error=failed`);
  }
}
