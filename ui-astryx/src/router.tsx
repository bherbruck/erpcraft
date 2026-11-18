import { createBrowserRouter, Navigate, Outlet } from "react-router";
import { Shell } from "./shell";
import { RequireSession, useSession } from "./session";
import { paths } from "./lib/paths";
import { SignInPage } from "./pages/sign-in";
import { SalesOrderListPage } from "./pages/sales-order-list";
import { SalesOrderPage } from "./pages/sales-order";
import { CustomerListPage } from "./pages/customer-list";
import { CustomerPage } from "./pages/customer";

function Home() {
  const { companies } = useSession();
  return <Navigate to={paths.salesOrders(companies[0]?.code ?? "-")} replace />;
}

export const router = createBrowserRouter([
  { path: "/sign-in", element: <SignInPage /> },
  {
    element: (
      <RequireSession>
        <Outlet />
      </RequireSession>
    ),
    children: [
      { path: "/", element: <Home /> },
      {
        path: "/c/:company",
        element: <Shell />,
        children: [
          { index: true, element: <Navigate to="sales-orders" replace /> },
          { path: "sales-orders", element: <SalesOrderListPage /> },
          { path: "sales-orders/new", element: <SalesOrderPage key="new" /> },
          { path: "sales-orders/:id", element: <SalesOrderPage /> },
          { path: "customers", element: <CustomerListPage /> },
          { path: "customers/new", element: <CustomerPage key="new" /> },
          { path: "customers/:id", element: <CustomerPage /> },
        ],
      },
    ],
  },
]);
