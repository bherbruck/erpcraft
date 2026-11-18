import { Button } from "@astryxdesign/core/Button";
import { Center } from "@astryxdesign/core/Center";
import { EmptyState } from "@astryxdesign/core/EmptyState";
import { useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { NO_COMPANY, paths } from "@/lib/paths";

export function NotFoundPage() {
  return (
    <Center height="100%" padding={6}>
      <EmptyState
        title={t("not_found.title")}
        description={t("not_found.body")}
        headingLevel={1}
        actions={<Button label={t("not_found.home")} href="/" />}
      />
    </Center>
  );
}

/** Reports, approvals, printing and command screens exist in ../ui and are not part of the spike. */
export function NotPortedPage() {
  const { code } = useCompany();
  return (
    <Center height="100%" padding={6}>
      <EmptyState
        title={t("not_found.title")}
        description={t("spike.not_ported")}
        headingLevel={1}
        actions={<Button label={t("not_found.home")} href={paths.home(code || NO_COMPANY)} />}
      />
    </Center>
  );
}
