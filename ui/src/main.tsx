import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { RouterProvider } from "react-router";
import { createAppRouter } from "./app/router";
import { finishSingleSignOn } from "./app/sso";
import { TooltipProvider } from "./components/ui/tooltip";
import "./index.css";

const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root element");

const mount = root;
// A single sign-on that ends on this page is finished before the app decides whether anyone is signed in.
void finishSingleSignOn().then(() => {
  createRoot(mount).render(
    <StrictMode>
      {/* Every tooltip of the app (field help, the keys of the lines) shares one provider. */}
      <TooltipProvider delayDuration={300}>
        <RouterProvider router={createAppRouter()} />
      </TooltipProvider>
    </StrictMode>,
  );
});
