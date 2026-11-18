import { Heading } from "@astryxdesign/core/Heading";
import { HStack } from "@astryxdesign/core/HStack";
import { Icon } from "@astryxdesign/core/Icon";
import { Layout, LayoutContent, LayoutHeader } from "@astryxdesign/core/Layout";
import { Link } from "@astryxdesign/core/Link";
import { StackItem } from "@astryxdesign/core/Stack";
import { Text } from "@astryxdesign/core/Text";
import { VStack } from "@astryxdesign/core/VStack";
import { ArrowLeft } from "lucide-react";
import { Fragment, type ReactNode } from "react";

/**
 * A screen, as Astryx's page templates lay one out (`astryx template detail-page`, `table-page`):
 * a pinned LayoutHeader with a back link above the title, the title with its status, one line of
 * metadata, the actions on the right and the tabs underneath; then the content on the same content
 * line (padding 4 throughout). Records cap their width; tables fill the region.
 */
export function Page({
  title,
  titleNote,
  back,
  badge,
  meta,
  actions,
  toolbar,
  tabs,
  padded = true,
  contentWidth,
  children,
}: {
  title: string;
  /** Quieter text after the title: a document's number, or "(no number yet)". */
  titleNote?: string;
  /** The list the page belongs to. Back in the browser still goes back one step. */
  back?: { label: string; href: string };
  badge?: ReactNode;
  /** Facts about the record, on one line under the title, separated by bullets. */
  meta?: ReactNode[];
  actions?: ReactNode;
  /** Controls under the title, such as filters. */
  toolbar?: ReactNode;
  /** A TabList under the title: the header then has no bottom padding, so the tabs sit on its divider. */
  tabs?: ReactNode;
  /** Off for a page whose table owns its inset (lists). */
  padded?: boolean;
  /** Cap for records and forms; tables fill the region. */
  contentWidth?: number;
  children: ReactNode;
}) {
  const facts = (meta ?? []).filter((fact) => fact !== null && fact !== undefined && fact !== false);
  return (
    <Layout
      height="fill"
      contentWidth={contentWidth}
      header={
        <LayoutHeader hasDivider padding={4} paddingBlockEnd={tabs ? 0 : 4}>
          <VStack gap={3}>
            <HStack gap={4} vAlign="start" wrap="wrap">
              <StackItem size="fill">
                <VStack gap={1}>
                  {back ? (
                    <Link href={back.href}>
                      <HStack gap={1} vAlign="center">
                        <Icon icon={ArrowLeft} size="sm" color="secondary" />
                        <Text color="secondary">{back.label}</Text>
                      </HStack>
                    </Link>
                  ) : null}
                  <HStack gap={2} vAlign="center" wrap="wrap">
                    <Heading level={1} maxLines={1}>
                      {title}
                      {titleNote ? <Text color="secondary" weight="normal">{` ${titleNote}`}</Text> : null}
                    </Heading>
                    {badge}
                  </HStack>
                  {facts.length > 0 ? (
                    <HStack gap={1} vAlign="center" wrap="wrap">
                      {facts.map((fact, index) => (
                        // biome-ignore lint/suspicious/noArrayIndexKey: the facts are a fixed list per page
                        <Fragment key={index}>
                          {index > 0 ? <Text color="secondary">·</Text> : null}
                          {typeof fact === "string" ? <Text color="secondary">{fact}</Text> : fact}
                        </Fragment>
                      ))}
                    </HStack>
                  ) : null}
                </VStack>
              </StackItem>
              {actions ? (
                <HStack gap={2} vAlign="center" wrap="wrap">
                  {actions}
                </HStack>
              ) : null}
            </HStack>
            {toolbar}
            {tabs}
          </VStack>
        </LayoutHeader>
      }
      content={<LayoutContent padding={padded ? 4 : 0}>{children}</LayoutContent>}
    />
  );
}
