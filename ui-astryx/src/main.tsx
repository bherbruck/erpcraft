import { LinkProvider } from "@astryxdesign/core/Link";
import { Theme } from "@astryxdesign/core/theme";
import { neutralTheme } from "@astryxdesign/theme-neutral/built";
import { type AnchorHTMLAttributes, forwardRef, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { Link, RouterProvider } from "react-router";
import { router } from "./router";
import "./globals.css";

/** Astryx renders every link through this, so links are client-side and open in a new tab normally. */
const RouterLink = forwardRef<HTMLAnchorElement, AnchorHTMLAttributes<HTMLAnchorElement> & { href?: string }>(
  function RouterLink({ href, ...rest }, ref) {
    if (!href || /^[a-z]+:/i.test(href)) return <a ref={ref} href={href} {...rest} />;
    return <Link ref={ref} to={href} {...rest} />;
  },
);

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <Theme theme={neutralTheme}>
        <LinkProvider component={RouterLink}>
          <RouterProvider router={router} />
        </LinkProvider>
      </Theme>
    </StrictMode>,
  );
}
