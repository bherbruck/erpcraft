import { Navigate } from "react-router";
import { useApp, useCompany } from "@/app/context";
import { navigation } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { NotFoundPage } from "./not-found";

/** The start page is the first screen of the menu. */
export function HomePage() {
  const { meta } = useApp();
  const { code } = useCompany();
  const first = navigation(meta)[0];
  const docType = first?.docTypes[0];
  const entity = first?.entities[0];
  if (docType) return <Navigate to={paths.docList(code, docType.name)} replace />;
  if (entity) return <Navigate to={paths.entityList(code, entity.name)} replace />;
  return <NotFoundPage />;
}

export default HomePage;
