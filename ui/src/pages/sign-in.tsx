import { type FormEvent, useState } from "react";
import { Navigate, useLocation, useNavigate, useSearchParams } from "react-router";
import { ApiError, api, session } from "@/api/client";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { type MessageKey, t } from "@/i18n/t";
import { useAsync } from "@/lib/use-async";

function returnTo(state: unknown): string {
  if (typeof state === "object" && state !== null && "from" in state && typeof state.from === "string") {
    // Only ever a path inside this app.
    if (state.from.startsWith("/") && !state.from.startsWith("//")) return state.from;
  }
  return "/";
}

// Why a single sign-on was refused, as the server names it in `?sso_error=`.
const SSO_ERRORS: Record<string, MessageKey> = {
  provider_unavailable: "sso.error.provider_unavailable",
  provider_refused: "sso.error.provider_refused",
  invalid_state: "sso.error.invalid_state",
  invalid_token: "sso.error.invalid_token",
  no_email: "sso.error.no_email",
  email_not_verified: "sso.error.email_not_verified",
  domain_not_allowed: "sso.error.domain_not_allowed",
  no_account: "sso.error.no_account",
  inactive: "sso.error.inactive",
  not_bound: "sso.error.not_bound",
  admin_not_allowed: "sso.error.admin_not_allowed",
  failed: "sso.error.failed",
};

/** Where a provider's sign-in starts. A plain navigation: the server sends the browser on to the provider. */
function ssoStart(slug: string, returnTo: string): string {
  return `/api/auth/sso/${encodeURIComponent(slug)}/start?return_to=${encodeURIComponent(returnTo)}`;
}

export function SignInPage() {
  const navigate = useNavigate();
  const location = useLocation();
  const [params] = useSearchParams();
  const ssoError = params.get("sso_error");
  const providers = useAsync("sso-providers", (signal) => api.ssoProviders(signal));
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (session.has() && !busy) return <Navigate to={returnTo(location.state)} replace />;

  async function submit(event: FormEvent): Promise<void> {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.login(email, password);
      navigate(returnTo(location.state), { replace: true });
    } catch (cause) {
      // The email stays; only the password is asked for again.
      setError(cause instanceof ApiError ? cause.message : t("error.unexpected"));
      setBusy(false);
    }
  }

  return (
    <main className="flex min-h-screen items-center justify-center bg-muted p-4">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <p className="text-sm font-semibold tracking-tight text-muted-foreground">{t("app.name")}</p>
          <CardTitle className="text-xl">{t("auth.title")}</CardTitle>
          <CardDescription>{t("auth.subtitle")}</CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={(event) => void submit(event)} className="space-y-4">
            {ssoError && !error ? (
              <Alert title={t("auth.failed")}>{t(SSO_ERRORS[ssoError] ?? "sso.error.failed")}</Alert>
            ) : null}
            {error ? <Alert title={t("auth.failed")}>{error}</Alert> : null}
            <div className="space-y-2">
              <Label htmlFor="email">{t("auth.email")}</Label>
              <Input
                id="email"
                type="email"
                autoComplete="username"
                required
                autoFocus
                value={email}
                onChange={(event) => setEmail(event.target.value)}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="password">{t("auth.password")}</Label>
              <Input
                id="password"
                type="password"
                autoComplete="current-password"
                required
                value={password}
                onChange={(event) => setPassword(event.target.value)}
              />
            </div>
            <Button type="submit" className="w-full" busy={busy}>
              {t("auth.sign_in")}
            </Button>
            {providers.data && providers.data.providers.length > 0 ? (
              <div className="space-y-2 border-t pt-4">
                <p className="text-center text-sm text-muted-foreground">{t("sso.or")}</p>
                {providers.data.providers.map((provider) => (
                  <Button key={provider.slug} asChild variant="outline" className="w-full">
                    <a href={ssoStart(provider.slug, returnTo(location.state))}>
                      {t("sso.sign_in_with", { name: provider.name })}
                    </a>
                  </Button>
                ))}
              </div>
            ) : null}
          </form>
        </CardContent>
      </Card>
    </main>
  );
}
