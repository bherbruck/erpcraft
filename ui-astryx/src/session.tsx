import { createContext, type ReactNode, useContext, useEffect, useState } from "react";
import { Navigate, useLocation } from "react-router";
import { api, session } from "@/api/client";
import type { EntityRecord, Me, Meta } from "@/api/types";
import { Center } from "@astryxdesign/core/Center";
import { Spinner } from "@astryxdesign/core/Spinner";
import { t } from "@/i18n/t";

export interface Company {
  id: string;
  code: string;
  name: string;
}

interface SessionValue {
  me: Me;
  meta: Meta;
  companies: Company[];
  signOut: () => Promise<void>;
}

const SessionContext = createContext<SessionValue | null>(null);

export function useSession(): SessionValue {
  const value = useContext(SessionContext);
  if (!value) throw new Error("useSession outside RequireSession");
  return value;
}

function asCompany(record: EntityRecord): Company {
  return { id: record.id, code: String(record.code ?? ""), name: String(record.name ?? "") };
}

/** Loads who is signed in, the registry and the companies, or sends the user to sign in. */
export function RequireSession({ children }: { children: ReactNode }) {
  const location = useLocation();
  const [value, setValue] = useState<SessionValue | null>(null);
  const [signedOut, setSignedOut] = useState(() => session.restore() === null);

  useEffect(() => {
    session.onUnauthorized(() => setSignedOut(true));
    if (signedOut) return;
    const controller = new AbortController();
    Promise.all([
      api.me(controller.signal),
      api.meta(controller.signal),
      api.run("entity.list", { entity: "company", limit: 500, sort: ["code"] }, { signal: controller.signal }),
    ]).then(
      ([me, meta, companies]) =>
        setValue({
          me,
          meta,
          companies: companies.items.map(asCompany),
          signOut: async () => {
            await api.logout();
            setSignedOut(true);
          },
        }),
      () => {
        if (!controller.signal.aborted) setSignedOut(true);
      },
    );
    return () => controller.abort();
  }, [signedOut]);

  if (signedOut) {
    const next = location.pathname + location.search;
    return <Navigate to={`/sign-in?next=${encodeURIComponent(next)}`} replace />;
  }
  if (!value) {
    return (
      <Center height="100dvh">
        <Spinner label={t("common.loading")} />
      </Center>
    );
  }
  return <SessionContext.Provider value={value}>{children}</SessionContext.Provider>;
}
