import { createBrowserRouter, Navigate, Outlet } from "react-router";
import { AppShell } from "@/components/app-shell";
import { NO_COMPANY, paths } from "@/lib/paths";
import { DocumentListPage } from "@/pages/document-list";
import { DocumentPage } from "@/pages/document-page";
import { EntityListPage } from "@/pages/entity-list";
import { EntityRecordPage } from "@/pages/entity-record";
import { HomePage } from "@/pages/home";
import { NotFoundPage, NotPortedPage } from "@/pages/not-found";
import { SignInPage } from "@/pages/sign-in";
import { useApp } from "./context";
import { SignedIn } from "./session";

function ToFirstCompany() {
  const { companies } = useApp();
  return <Navigate to={paths.home(companies[0]?.code ?? NO_COMPANY)} replace />;
}

/** The same routes as ../ui, so links and bookmarks work in both. */
export const router = createBrowserRouter([
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
          { path: "d/:docType", element: <DocumentListPage /> },
          { path: "d/:docType/new", element: <DocumentPage /> },
          { path: "d/:docType/:id", element: <DocumentPage /> },
          { path: "*", element: <NotPortedPage /> },
        ],
      },
      { path: "*", element: <NotFoundPage /> },
    ],
  },
]);
