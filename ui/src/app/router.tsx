import { createBrowserRouter, Navigate, Outlet } from "react-router";
import { AppShell } from "@/components/app-shell";
import { NO_COMPANY, paths } from "@/lib/paths";
import { ApprovalPage, ApprovalsPage } from "@/pages/approvals";
import { CommandPage } from "@/pages/command-page";
import { CompanySetupPage } from "@/pages/company-setup";
import { DocumentListPage } from "@/pages/document-list";
import { DocumentPage } from "@/pages/document-page";
import { EntityListPage } from "@/pages/entity-list";
import { EntityRecordPage } from "@/pages/entity-record";
import { HomePage } from "@/pages/home";
import { NotFoundPage } from "@/pages/not-found";
import { PartyEditPage } from "@/pages/party-details";
import { PrintPage } from "@/pages/print-page";
import { ReportPage } from "@/pages/report-page";
import { SignInPage } from "@/pages/sign-in";
import { useApp } from "./context";
import { SignedIn } from "./session";

/** "/" leads to the first company, or to the place where one is created. */
function ToFirstCompany() {
  const { companies } = useApp();
  return <Navigate to={paths.home(companies[0]?.code ?? NO_COMPANY)} replace />;
}

/** The app's routes. Created once the address is final (see `finishSingleSignOn`). */
export const createAppRouter = () =>
  createBrowserRouter([
    { path: "/sign-in", element: <SignInPage /> },
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
            { index: true, element: <HomePage /> },
            { path: "e/:entity", element: <EntityListPage /> },
            { path: "e/:entity/new", element: <EntityRecordPage /> },
            { path: "e/:entity/:id", element: <EntityRecordPage /> },
            { path: "e/:entity/:id/party", element: <PartyEditPage /> },
            { path: "d/:docType", element: <DocumentListPage /> },
            { path: "d/:docType/new", element: <DocumentPage /> },
            { path: "d/:docType/:id", element: <DocumentPage /> },
            { path: "d/:docType/:id/print", element: <PrintPage /> },
            { path: "r/:report", element: <ReportPage /> },
            { path: "setup", element: <CompanySetupPage /> },
            { path: "approvals", element: <ApprovalsPage /> },
            { path: "approvals/:id", element: <ApprovalPage /> },
            { path: "run/:command", element: <CommandPage /> },
            { path: "*", element: <NotFoundPage /> },
          ],
        },
        { path: "*", element: <NotFoundPage /> },
      ],
    },
  ]);
