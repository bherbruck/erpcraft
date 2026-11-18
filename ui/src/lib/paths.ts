// Every screen is a URL. The current company is the first part of the path.

/** Stands in for the company while none exists yet. */
export const NO_COMPANY = "_";

const enc = encodeURIComponent;

export const paths = {
  signIn: (): string => "/sign-in",
  home: (company: string): string => `/c/${enc(company)}`,
  entityList: (company: string, entity: string): string => `/c/${enc(company)}/e/${enc(entity)}`,
  entityNew: (company: string, entity: string): string => `/c/${enc(company)}/e/${enc(entity)}/new`,
  entityRecord: (company: string, entity: string, id: string): string =>
    `/c/${enc(company)}/e/${enc(entity)}/${enc(id)}`,
  docList: (company: string, docType: string): string => `/c/${enc(company)}/d/${enc(docType)}`,
  docNew: (company: string, docType: string): string => `/c/${enc(company)}/d/${enc(docType)}/new`,
  doc: (company: string, docType: string, id: string): string => `/c/${enc(company)}/d/${enc(docType)}/${enc(id)}`,
  print: (company: string, docType: string, id: string): string =>
    `/c/${enc(company)}/d/${enc(docType)}/${enc(id)}/print`,
  /** A report, named by its command: `gl.trial_balance`. */
  /** A screen that runs one command, such as importing a bank statement. */
  run: (company: string, command: string): string => `/c/${enc(company)}/run/${enc(command)}`,
  approvals: (company: string): string => `/c/${enc(company)}/approvals`,
  approval: (company: string, id: string): string => `/c/${enc(company)}/approvals/${enc(id)}`,
  /** A new company from a template. */
  setup: (company: string): string => `/c/${enc(company)}/setup`,
  report: (company: string, command: string): string => `/c/${enc(company)}/r/${enc(command)}`,
};

/**
 * The same screen in another company: the list or report the user is on.
 * A record belongs to one company, so a record page leads to its list.
 */
export function pathInCompany(pathname: string, company: string): string {
  const [, c, , kind, name] = pathname.split("/");
  if (c !== "c" || !kind || !name) return paths.home(company);
  return `/c/${enc(company)}/${kind}/${name}`;
}
