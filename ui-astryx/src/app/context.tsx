import { createContext, useContext } from "react";
import { useParams } from "react-router";
import type { Me, Meta } from "@/api/types";
import { NO_COMPANY } from "@/lib/paths";

export interface Company {
  id: string;
  code: string;
  name: string;
  base_currency: string;
}

export interface AppData {
  /** The server's description of itself and of what this user may do. */
  meta: Meta;
  me: Me;
  companies: Company[];
  /** Decimals of each currency, by code. */
  currencyDecimals: Record<string, number>;
  reloadCompanies: () => void;
  signOut: () => void;
}

export const AppContext = createContext<AppData | null>(null);

export function useApp(): AppData {
  const app = useContext(AppContext);
  if (!app) throw new Error("useApp must be used inside the signed-in part of the app");
  return app;
}

/** The company named in the URL. `company` is null when the URL names none that the user can see. */
export function useCompany(): { code: string; company: Company | null } {
  const { companies } = useApp();
  const params = useParams();
  const code = params.company ?? NO_COMPANY;
  return { code, company: companies.find((company) => company.code === code) ?? null };
}
