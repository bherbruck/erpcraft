import { Heading } from "@astryxdesign/core/Heading";
import { HStack } from "@astryxdesign/core/HStack";
import { Icon } from "@astryxdesign/core/Icon";
import { IconButton } from "@astryxdesign/core/IconButton";
import { Layout, LayoutContent, LayoutHeader } from "@astryxdesign/core/Layout";
import { Text } from "@astryxdesign/core/Text";
import { VStack } from "@astryxdesign/core/VStack";
import { ArrowLeft } from "lucide-react";
import type { ReactNode } from "react";
import { useNavigate } from "react-router";
import { t } from "@/i18n/t";

/**
 * A screen: a header that stays put (title, status, actions) over content that
 * scrolls. Astryx's Layout with its regions as slots.
 */
export function Page({
  title,
  kicker,
  badge,
  actions,
  toolbar,
  back = false,
  padded = true,
  children,
}: {
  title: string;
  /** A small line above the title: what kind of record this is. */
  kicker?: string;
  badge?: ReactNode;
  actions?: ReactNode;
  /** Controls under the title, such as search and filters. */
  toolbar?: ReactNode;
  back?: boolean;
  padded?: boolean;
  children: ReactNode;
}) {
  const navigate = useNavigate();
  return (
    <Layout
      height="fill"
      header={
        <LayoutHeader hasDivider padding={3}>
          <VStack gap={3}>
            <HStack hAlign="between" vAlign="center" gap={3} wrap="wrap">
              <HStack gap={2} vAlign="center">
                {back ? (
                  <IconButton
                    label={t("common.back")}
                    icon={<Icon icon={ArrowLeft} />}
                    variant="ghost"
                    size="sm"
                    onClick={() => navigate(-1)}
                  />
                ) : null}
                <VStack gap={0.5}>
                  {kicker ? <Text type="supporting">{kicker}</Text> : null}
                  <HStack gap={2} vAlign="center" wrap="wrap">
                    <Heading level={1}>{title}</Heading>
                    {badge}
                  </HStack>
                </VStack>
              </HStack>
              {actions ? (
                <HStack gap={2} vAlign="center" wrap="wrap">
                  {actions}
                </HStack>
              ) : null}
            </HStack>
            {toolbar}
          </VStack>
        </LayoutHeader>
      }
      content={<LayoutContent padding={padded ? 4 : 0}>{children}</LayoutContent>}
    />
  );
}
