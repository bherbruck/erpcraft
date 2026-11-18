import { type ReactNode, useCallback, useEffect, useMemo, useState } from "react";
import { Navigate, useLocation, useNavigate } from "react-router";
import { api, session } from "@/api/client";
import type { EntityRecord } from "@/api/types";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { Skeleton } from "@/components/ui/skeleton";
import { t } from "@/i18n/t";
import { forgetLinkLabels, seedRecords, setLabelRegistry } from "@/lib/link-labels";
import { COMPANY, CURRENCY } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useAsync } from "@/lib/use-async";
import { AppContext, type AppData, type Company } from "./context";

/**
 * In development only, a token can be handed to the page in the URL fragment
 * (`#token=…`), which is how the screenshot script signs in. The fragment is
 * removed from the address at once. Production builds ignore it.
 */
function adoptDevToken(): void {
  if (!import.meta.env.DEV) return;
  const match = /^#token=([A-Za-z0-9._~+/=-]+)$/.exec(window.location.hash);
  if (!match?.[1]) return;
  session.set(match[1]);
  window.history.replaceState(null, "", window.location.pathname + window.location.search);
}

adoptDevToken();
session.restore();

function text(record: EntityRecord, field: string): string {
  const value = record[field];
  return typeof value === "string" ? value : "";
}

/** Everything behind the sign-in: loads who the user is and what they may do. */
export function SignedIn({ children }: { children: ReactNode }) {
  const navigate = useNavigate();
  const location = useLocation();
  const [signedIn, setSignedIn] = useState(session.has());

  useEffect(() => {
    // A 401 anywhere drops the token; the user signs in again and returns here.
    session.onUnauthorized(() => setSignedIn(false));
    return () => session.onUnauthorized(() => {});
  }, []);

  const base = useAsync(signedIn ? "base" : null, async (signal) => {
    const [meta, me] = await Promise.all([api.meta(signal), api.me(signal)]);
    return { meta, me };
  });
  const lists = useAsync(signedIn ? "lists" : null, async (signal) => {
    const [companies, currencies] = await Promise.all([
      api.run("entity.list", { entity: COMPANY, limit: 500 }, { signal }),
      api.run("entity.list", { entity: CURRENCY, limit: 500 }, { signal }).catch(() => ({ items: [], total: 0 })),
    ]);
    return { companies: companies.items, currencies: currencies.items };
  });

  const signOut = useCallback(() => {
    void api.logout().then(() => {
      forgetLinkLabels();
      setSignedIn(false);
      navigate(paths.signIn());
    });
  }, [navigate]);

  const app = useMemo<AppData | null>(() => {
    if (!base.data || !lists.data) return null;
    setLabelRegistry(base.data.meta);
    // The companies are read already: their names need no request of their own
    // (the company switcher shows one on every page, by code).
    seedRecords(COMPANY, "code", lists.data.companies);
    seedRecords(COMPANY, "id", lists.data.companies);
    const companies: Company[] = lists.data.companies.map((record) => ({
      id: record.id,
      code: text(record, "code"),
      name: text(record, "name"),
      base_currency: text(record, "base_currency"),
    }));
    const currencyDecimals: Record<string, number> = {};
    for (const currency of lists.data.currencies) {
      if (typeof currency.decimals === "number") currencyDecimals[text(currency, "code")] = currency.decimals;
    }
    return { ...base.data, companies, currencyDecimals, reloadCompanies: lists.reload, signOut };
  }, [base.data, lists.data, lists.reload, signOut]);

  if (!signedIn) {
    const from = location.pathname + location.search;
    return <Navigate to={paths.signIn()} replace state={{ from }} />;
  }

  const error = base.error ?? lists.error;
  if (!app) {
    return (
      <div className="mx-auto flex min-h-screen max-w-form flex-col justify-center gap-3 p-6">
        {error ? (
          <Alert
            title={t("app.load_failed")}
            actions={
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  base.reload();
                  lists.reload();
                }}
              >
                {t("action.retry")}
              </Button>
            }
          >
            {error.message}
          </Alert>
        ) : (
          <output aria-label={t("app.loading")} className="space-y-3">
            <Skeleton className="h-6 w-1/3" />
            <Skeleton className="w-full" />
            <Skeleton className="w-2/3" />
          </output>
        )}
      </div>
    );
  }
  return <AppContext.Provider value={app}>{children}</AppContext.Provider>;
}
