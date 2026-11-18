import { createBrowserRouter, Navigate, Outlet } from "react-router";
import { AppShell } from "@/components/app-shell";
import { NO_COMPANY, paths } from "@/lib/paths";
import { NotFoundPage, NotPortedPage } from "@/pages/not-found";
import { useApp } from "./context";
import { SignedIn } from "./session";

function ToFirstCompany() {
  const { companies } = useApp();
  return <Navigate to={paths.home(companies[0]?.code ?? NO_COMPANY)} replace />;
}

// Each screen is its own chunk, loaded when it is first opened: the shell does not carry
// the table, the filter bar, the date picker or the dialogs until a page needs them.
// A chunk that fails to arrive (a dropped connection) is asked for once more before the error page.
async function retry<T>(load: () => Promise<T>): Promise<T> {
  try {
    return await load();
  } catch {
    await new Promise((done) => setTimeout(done, 400));
    return load();
  }
}
const page = (load: () => Promise<{ default: React.ComponentType }>) => async () => ({ Component: (await retry(load)).default });

/** The same routes as ../ui, so links and bookmarks work in both. */
export const router = createBrowserRouter([
  { path: "/sign-in", lazy: page(() => import("@/pages/sign-in")) },
  {
    element: (
      <SignedIn>
        <Outlet />
      </SignedIn>
    ),
    children: [
      { index: true, element: <ToFirstCompany /> },
      {
        path: "/c/:company",
        element: <AppShell />,
        children: [
          { index: true, lazy: page(() => import("@/pages/home")) },
          { path: "e/:entity", lazy: page(() => import("@/pages/entity-list")) },
          { path: "e/:entity/new", lazy: page(() => import("@/pages/entity-record")) },
          { path: "e/:entity/:id", lazy: page(() => import("@/pages/entity-record")) },
          { path: "d/:docType", lazy: page(() => import("@/pages/document-list")) },
          { path: "d/:docType/new", lazy: page(() => import("@/pages/document-page")) },
          { path: "d/:docType/:id", lazy: page(() => import("@/pages/document-page")) },
          { path: "d/:docType/:id/print", lazy: page(() => import("@/pages/print-page")) },
          { path: "r/:report", lazy: page(() => import("@/pages/report-page")) },
          { path: "approvals", lazy: async () => ({ Component: (await retry(() => import("@/pages/approvals"))).ApprovalsPage }) },
          { path: "approvals/:id", lazy: async () => ({ Component: (await retry(() => import("@/pages/approvals"))).ApprovalPage }) },
          { path: "setup", lazy: page(() => import("@/pages/company-setup")) },
          { path: "run/:command", lazy: page(() => import("@/pages/command-page")) },
          { path: "*", element: <NotPortedPage /> },
        ],
      },
      { path: "*", element: <NotFoundPage /> },
    ],
  },
]);
