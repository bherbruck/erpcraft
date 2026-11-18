import { Link } from "react-router";
import { useCompany } from "@/app/context";
import { Button } from "@/components/button";
import { t } from "@/i18n/t";
import { paths } from "@/lib/paths";

/** Also shown for what the user may not see: the two are not told apart. */
export function NotFoundPage() {
  const { code } = useCompany();
  return (
    <div className="mx-auto max-w-form space-y-3 py-16 text-center">
      <h1 className="text-lg font-semibold">{t("not_found.title")}</h1>
      <p className="text-sm text-muted-foreground">{t("not_found.body")}</p>
      <Button asChild variant="outline">
        <Link to={paths.home(code)}>{t("not_found.home")}</Link>
      </Button>
    </div>
  );
}
