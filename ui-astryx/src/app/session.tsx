import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Center } from "@astryxdesign/core/Center";
import { Spinner } from "@astryxdesign/core/Spinner";
import { VStack } from "@astryxdesign/core/VStack";
import { type ReactNode, useCallback, useEffect, useMemo, useState } from "react";
import { Navigate, useLocation, useNavigate } from "react-router";
import { api, session } from "@/api/client";
import type { EntityRecord } from "@/api/types";
import { t } from "@/i18n/t";
import { forgetLinkLabels, seedRecords, setLabelRegistry } from "@/lib/link-labels";
import { COMPANY, CURRENCY } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useAsync } from "@/lib/use-async";
import { AppContext, type AppData, type Company } from "./context";

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
      <Center height="100dvh" padding={4}>
        {error ? (
          <VStack maxWidth={480}>
            <Banner
              status="error"
              title={t("app.load_failed")}
              description={error.message}
              endContent={
                <Button
                  label={t("action.retry")}
                  size="sm"
                  onClick={() => {
                    base.reload();
                    lists.reload();
                  }}
                />
              }
            />
          </VStack>
        ) : (
          <Spinner label={t("app.loading")} />
        )}
      </Center>
    );
  }
  return <AppContext.Provider value={app}>{children}</AppContext.Provider>;
}
