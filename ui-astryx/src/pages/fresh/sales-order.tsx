// The sales order page, rebuilt guidance-first: scaffolded from Astryx's `detail-page`
// template (`astryx build "sales order document page..."` named it), keeping its frame,
// header, gap and padding, with the content replaced by the order.
//
// Responsive contract (astryx docs layout/responsive):
//   >1024  header actions beside the title; details in three columns over the lines table
//   <=1024 actions drop to their own row under the metadata (useMediaQuery), as the template does
//   any    the details Grid reflows ({minWidth: 240, max: 3}); the lines Table scrolls sideways
//          once its column floors do not fit, rather than squeezing every column
// The template's side panel is dropped: its region would compete for the width the lines
// need (astryx docs layout/responsive: "Drop a region rather than let it compete").

import { AlertDialog } from "@astryxdesign/core/AlertDialog";
import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { DateInput } from "@astryxdesign/core/DateInput";
import { DropdownMenu, DropdownMenuItem } from "@astryxdesign/core/DropdownMenu";
import { Grid, GridSpan } from "@astryxdesign/core/Grid";
import { useMediaQuery } from "@astryxdesign/core/hooks";
import { Icon } from "@astryxdesign/core/Icon";
import { IconButton } from "@astryxdesign/core/IconButton";
import { HStack, Layout, LayoutContent, LayoutHeader, Section, StackItem, VStack } from "@astryxdesign/core/Layout";
import { Link } from "@astryxdesign/core/Link";
import { List, ListItem } from "@astryxdesign/core/List";
import { MetadataList, MetadataListItem } from "@astryxdesign/core/MetadataList";
import { Selector } from "@astryxdesign/core/Selector";
import { Skeleton } from "@astryxdesign/core/Skeleton";
import { Spinner } from "@astryxdesign/core/Spinner";
import { StatusDot } from "@astryxdesign/core/StatusDot";
import { Tab, TabList } from "@astryxdesign/core/TabList";
import { Heading, Text } from "@astryxdesign/core/Text";
import { TextInput } from "@astryxdesign/core/TextInput";
import type { ISODateString } from "@astryxdesign/core/utils";
import { ArrowLeft, Printer, Trash2 } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useBlocker, useNavigate, useParams } from "react-router";
import { ApiError, api } from "@/api/client";
import type { DocTypeMeta, DocumentRecord, FieldMeta, Json, JsonObject, StateMeta } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { formatCalendarDate, formatTimestamp } from "@/lib/dates";
import { formatDecimal } from "@/lib/decimal";
import { canDoc, canRun, findDocType, recordActions, type ResourceAction } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "../not-found";
import { addressLine, RecordLookup } from "./lookups";
import { OrderLines } from "./order-lines";
import {
  bodyOf,
  blankLine,
  blankOrder,
  type HeaderField,
  isUntouched,
  type LineField,
  NO_PROBLEMS,
  type OrderForm,
  orderFromDocument,
  placeProblem,
  type Problems,
  snapshot,
} from "./order-model";

const SALES_ORDER = "sales_order";

/** The fresh routes, beside the generated document routes. */
export const freshPaths = {
  order: (company: string, id: string) => `/c/${encodeURIComponent(company)}/fresh/sales_order/${encodeURIComponent(id)}`,
  newOrder: (company: string) => `/c/${encodeURIComponent(company)}/fresh/sales_order/new`,
};

const isObject = (value: Json | undefined): value is JsonObject =>
  typeof value === "object" && value !== null && !Array.isArray(value);

export default function FreshSalesOrderPage() {
  const { meta } = useApp();
  const { company } = useCompany();
  const params = useParams();
  const docType = findDocType(meta, SALES_ORDER);
  const id = params.id && params.id !== "new" ? params.id : null;
  if (!docType || !canDoc(meta, SALES_ORDER, "read")) return <NotFoundPage />;
  if (!id && (!canDoc(meta, SALES_ORDER, "create") || !company)) return <NotFoundPage />;
  return <SalesOrder key={id ?? "new"} docType={docType} id={id} />;
}

// ─── Status ────────────────────────────────────────────────────────────────

const TONE: Record<string, "success" | "warning" | "error" | "accent" | "neutral"> = {
  success: "success",
  warning: "warning",
  danger: "error",
  error: "error",
  info: "accent",
  neutral: "neutral",
};

/** Status is a StatusDot with its word, never a colour alone. */
function Status({ state }: { state: StateMeta | undefined }) {
  if (!state) return null;
  return (
    <HStack gap={1} vAlign="center">
      <StatusDot variant={TONE[state.tone] ?? "neutral"} label={state.label} />
      <Text>{state.label}</Text>
    </HStack>
  );
}

// ─── Bullet separator (from the template) ──────────────────────────────────

function Bullet() {
  return (
    <Text type="supporting" color="secondary">
      {"・"}
    </Text>
  );
}

// ─── Page header (the template's, with the order's facts) ──────────────────

function PageHeader({
  back,
  title,
  facts,
  actions,
  tab,
  onTab,
  showTabs,
  isNarrow,
}: {
  back: { label: string; href: string };
  title: string;
  facts: React.ReactNode[];
  actions: React.ReactNode;
  tab: string;
  onTab: (value: string) => void;
  showTabs: boolean;
  isNarrow: boolean;
}) {
  const shown = facts.filter(Boolean);
  return (
    <LayoutHeader hasDivider padding={4} paddingBlockEnd={showTabs ? 0 : 4}>
      <VStack gap={3}>
        <HStack gap={4} vAlign="start">
          <StackItem size="fill">
            <VStack gap={0}>
              <Link href={back.href} color="secondary">
                <HStack gap={1} vAlign="center">
                  <Icon icon={ArrowLeft} size="sm" color="inherit" />
                  {back.label}
                </HStack>
              </Link>
              <VStack gap={0}>
                <Heading level={1} maxLines={1}>
                  {title}
                </Heading>
                <HStack gap={1} vAlign="center" wrap="wrap">
                  {shown.map((fact, index) => (
                    <HStack key={index} gap={1} vAlign="center">
                      {index > 0 ? <Bullet /> : null}
                      {fact}
                    </HStack>
                  ))}
                </HStack>
              </VStack>
            </VStack>
          </StackItem>
          {!isNarrow && (
            <HStack gap={2} vAlign="center">
              {actions}
            </HStack>
          )}
        </HStack>
        {isNarrow && (
          <HStack gap={2} vAlign="center" wrap="wrap">
            {actions}
          </HStack>
        )}
        {showTabs ? (
          <TabList value={tab} onChange={onTab} size="lg" isFullBleed>
            <Tab value="order" label={t("document.tab")} />
            <Tab value="history" label={t("history.title")} />
          </TabList>
        ) : null}
      </VStack>
    </LayoutHeader>
  );
}

// ─── Totals (the template's invoice block: a MetadataList of figures) ──────

function Totals({ docType, summary, decimals }: { docType: DocTypeMeta; summary: JsonObject; decimals: number }) {
  const figures = docType.summary.filter((field) => field.type.kind === "decimal");
  if (figures.length === 0) return null;
  const currency = typeof summary.currency === "string" ? summary.currency : "";
  return (
    <HStack hAlign="end">
      <VStack gap={2} width={360}>
        <MetadataList label={{ position: "start", width: 160 }}>
          {[...figures.slice(1), ...figures.slice(0, 1)].map((field) => {
            const value = summary[field.name];
            const total = field === figures[0];
            return (
              <MetadataListItem key={field.name} label={total ? `${field.label} (${currency})` : field.label}>
                <HStack hAlign="end">
                  <Text hasTabularNumbers weight={total ? "semibold" : undefined}>
                    {typeof value === "string" ? formatDecimal(value, { minFraction: decimals }) : t("document.not_known_yet")}
                  </Text>
                </HStack>
              </MetadataListItem>
            );
          })}
        </MetadataList>
      </VStack>
    </HStack>
  );
}

// ─── History (rows, as the layout guidance renders a collection) ───────────

function History({ id }: { id: string }) {
  const history = useAsync(`history|${id}`, (signal) => api.run("audit.history", { resource: SALES_ORDER, id }, { signal }));
  if (history.loading && !history.data) return <Skeleton height={120} />;
  if (history.error) return <Banner status="error" title={t("record.load_failed")} description={history.error.message} />;
  const items = history.data?.items ?? [];
  if (items.length === 0) return <Text color="secondary">{t("history.empty")}</Text>;
  return (
    <Section padding={0}>
      <List hasDividers>
        {items.map((entry) => (
          <ListItem
            key={entry.id}
            label={entry.command}
            description={`${entry.user_name ?? ""} · ${formatTimestamp(entry.at)}`}
            endContent={<Text color={entry.outcome === "ok" ? "secondary" : "primary"}>{entry.outcome === "ok" ? t("history.ok") : t("history.error")}</Text>}
          />
        ))}
      </List>
    </Section>
  );
}

// ─── Addresses (a short list per customer: a Selector) ─────────────────────

function AddressSelector({
  field,
  kind,
  customerId,
  value,
  isDisabled,
  error,
  onChange,
}: {
  field: FieldMeta;
  kind: "billing" | "shipping";
  customerId: string;
  value: string;
  isDisabled: boolean;
  error?: string;
  onChange: (value: string) => void;
}) {
  const party = useAsync(customerId ? `party|${customerId}|${kind}` : null, (signal) =>
    api.run("party.get", { party_type: "customer", party_id: customerId, address_kind: kind }, { signal }),
  );
  const addresses = (party.data?.addresses ?? []).filter((address) => address.kind === kind);
  const options = addresses.flatMap((address) =>
    address.id ? [{ value: address.id, label: addressLine(address as unknown as JsonObject) }] : [],
  );
  return (
    <Selector
      label={field.label}
      options={options}
      value={value || null}
      onChange={(next: string | null) => onChange(next ?? "")}
      hasClear
      placeholder={t("fresh.address_none")}
      width="100%"
      isLoading={party.loading}
      isDisabled={isDisabled || !customerId}
      disabledMessage={!customerId ? t("fresh.address_pick_customer") : undefined}
      labelTooltip={field.help ?? undefined}
      status={error ? { type: "error", message: error } : undefined}
    />
  );
}

// ─── The page ──────────────────────────────────────────────────────────────

function SalesOrder({ docType, id }: { docType: DocTypeMeta; id: string | null }) {
  const { meta, currencyDecimals } = useApp();
  const { code, company } = useCompany();
  const navigate = useNavigate();
  const url = useUrlState();
  const isNarrow = useMediaQuery("(max-width: 1024px)");
  const isNew = id === null;
  const tab = url.get("tab") === "history" && !isNew ? "history" : "order";
  const lines = docType.collections.find((collection) => collection.name === "lines");

  const loaded = useAsync(id ? `${SALES_ORDER}|${id}` : null, (signal) =>
    api.run("document.get", { doc_type: SALES_ORDER, id: id ?? "" }, { signal }),
  );
  const [doc, setDoc] = useState<DocumentRecord | null>(null);
  const [form, setForm] = useState<OrderForm>(blankOrder);
  const [saved, setSaved] = useState(() => snapshot(blankOrder()));
  const [problems, setProblems] = useState<Problems>(NO_PROBLEMS);
  const [formError, setFormError] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const [saving, setSaving] = useState(false);
  const [summary, setSummary] = useState<JsonObject>({});
  const [nets, setNets] = useState<(string | null)[] | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [confirming, setConfirming] = useState<ResourceAction | "delete" | null>(null);
  const [running, setRunning] = useState<string | null>(null);
  const [focusKey, setFocusKey] = useState<string | null>(null);

  function adopt(next: DocumentRecord): void {
    const nextForm = orderFromDocument(next);
    setDoc(next);
    setForm(nextForm);
    setSaved(snapshot(nextForm));
    setSummary(isObject(next.summary) ? next.summary : {});
    setNets(null);
    setStale(false);
  }

  // biome-ignore lint/correctness/useExhaustiveDependencies: adopt only what was loaded
  useEffect(() => {
    if (loaded.data) adopt(loaded.data);
  }, [loaded.data]);

  const locked = doc?.locked ?? false;
  const canWrite = isNew ? canDoc(meta, SALES_ORDER, "create") : !locked && canDoc(meta, SALES_ORDER, "update");
  const current = snapshot(form);
  const dirty = canWrite && current !== saved;
  const companyId = doc?.company_id ?? company?.id ?? null;
  const currency = form.header.currency || company?.base_currency || "";
  const decimals = currencyDecimals[currency] ?? 2;

  // One idempotency key per payload: a double click or a retry cannot save twice.
  const keyRef = useRef({ payload: "", key: newKey() });
  const keyFor = (payload: string): string => {
    if (keyRef.current.payload !== payload) keyRef.current = { payload, key: newKey() };
    return keyRef.current.key;
  };

  // The server works out the totals: a preview of the same save, nothing kept.
  // biome-ignore lint/correctness/useExhaustiveDependencies: the snapshot names the form
  useEffect(() => {
    if (!dirty || !companyId) return;
    const controller = new AbortController();
    const timer = setTimeout(async () => {
      setPreviewing(true);
      try {
        const body = bodyOf(form);
        const result = doc
          ? await api.preview(
              "document.update",
              { doc_type: SALES_ORDER, id: doc.id, version: doc.version, body, doc_date: form.docDate },
              { signal: controller.signal },
            )
          : await api.preview(
              "document.create",
              { doc_type: SALES_ORDER, company_id: companyId, doc_date: form.docDate, body },
              { signal: controller.signal },
            );
        if (isObject(result) && isObject(result.summary)) setSummary(result.summary);
        if (isObject(result) && isObject(result.body) && Array.isArray(result.body.lines)) {
          const worked = result.body.lines.filter(isObject).map((line) => (typeof line.net_amount === "string" ? line.net_amount : null));
          // The preview leaves out blank lines; they get no amount.
          let next = 0;
          setNets(form.lines.map((line) => (isUntouched(line) ? null : (worked[next++] ?? null))));
        }
      } catch {
        // An unfinished line cannot be worked out yet; the last totals stay.
      } finally {
        if (!controller.signal.aborted) setPreviewing(false);
      }
    }, 400);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [current]);

  async function save(): Promise<void> {
    if (!canWrite || saving || !companyId || (!dirty && !isNew)) return;
    setSaving(true);
    setFormError(null);
    setProblems(NO_PROBLEMS);
    const body = bodyOf(form);
    try {
      if (!doc) {
        const payload = { doc_type: SALES_ORDER, company_id: companyId, doc_date: form.docDate, memo: form.memo || null, body };
        const created = await api.run("document.create", payload, { idempotencyKey: keyFor(JSON.stringify(payload)) });
        adopt(created);
        allowLeave.current = true;
        navigate(freshPaths.order(code, created.id), { replace: true });
      } else {
        const payload = { doc_type: SALES_ORDER, id: doc.id, version: doc.version, body, doc_date: form.docDate, memo: form.memo };
        adopt(await api.run("document.update", payload, { idempotencyKey: keyFor(JSON.stringify(payload)) }));
      }
    } catch (cause) {
      const error = cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected"));
      if (error.isStale) setStale(true);
      else {
        // Everything typed stays; the error goes to its field or line, in the server's words.
        const placed = placeProblem(error);
        if (placed) setProblems(placed);
        setFormError(placed ? t("spike.see_marked_field") : error.message);
      }
    }
    setSaving(false);
  }

  // Ctrl+S saves.
  const saveRef = useRef(save);
  saveRef.current = save;
  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        if (!document.querySelector("[role=alertdialog], dialog[open]")) void saveRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Leaving with unsaved changes asks first.
  const allowLeave = useRef(false);
  const blocker = useBlocker(({ currentLocation, nextLocation }) => {
    if (allowLeave.current) {
      allowLeave.current = false;
      return false;
    }
    return dirty && currentLocation.pathname !== nextLocation.pathname;
  });
  useEffect(() => {
    if (!dirty) return;
    const onUnload = (event: BeforeUnloadEvent) => event.preventDefault();
    window.addEventListener("beforeunload", onUnload);
    return () => window.removeEventListener("beforeunload", onUnload);
  }, [dirty]);

  const actions = useMemo(() => (doc ? recordActions(meta, SALES_ORDER, doc.status) : []), [meta, doc]);

  async function run(action: ResourceAction): Promise<void> {
    if (!doc) return;
    setRunning(action.command.name);
    setFormError(null);
    try {
      const result = await api.runNamed(action.command.name, { id: doc.id }, { idempotencyKey: keyFor(`${action.command.name}|${doc.id}|${doc.version}`) });
      setConfirming(null);
      const created = isObject(result) && typeof result.id === "string" && result.id !== doc.id ? result.id : null;
      if (action.creates && created) navigate(paths.doc(code, action.creates, created));
      else loaded.reload();
    } catch (cause) {
      setConfirming(null);
      setFormError(`${t("fresh.action_failed", { action: action.label })}: ${cause instanceof ApiError ? cause.message : String(cause)}`);
    }
    setRunning(null);
  }

  async function remove(): Promise<void> {
    if (!doc) return;
    setRunning("delete");
    try {
      await api.run("document.delete", { doc_type: SALES_ORDER, id: doc.id });
      allowLeave.current = true;
      navigate(paths.docList(code, SALES_ORDER), { replace: true });
    } catch (cause) {
      setConfirming(null);
      setRunning(null);
      setFormError(cause instanceof ApiError ? cause.message : String(cause));
    }
  }

  // ── Loading and failure keep the page's shape. ──
  if (loaded.error && !doc) {
    return (
      <VStack padding={4}>
        <Banner status="error" title={t("record.load_failed")} description={loaded.error.message} />
      </VStack>
    );
  }

  const state = docType.states.find((candidate) => candidate.name === (doc?.status ?? docType.initial_state));
  const title = doc ? (doc.number ?? `${docType.label} ${t("document.unnumbered")}`) : t("record.new", { label: docType.label });
  const field = (name: string) => docType.fields.find((candidate) => candidate.name === name);
  const headerLabel = (name: string, fallback: string) => meta.document_header.find((f) => f.name === name)?.label ?? fallback;
  const setHeader = (name: HeaderField, value: string) => setForm((previous) => ({ ...previous, header: { ...previous.header, [name]: value } }));
  const setLine = (key: string, change: Partial<Record<LineField, string>>) =>
    setForm((previous) => ({
      ...previous,
      lines: previous.lines.map((line) => (line.key === key ? { ...line, values: { ...line.values, ...change } } : line)),
    }));
  const workflow = actions.filter((action) => !action.destructive);
  const destructive = actions.filter((action) => action.destructive);
  const canDelete = doc !== null && !locked && doc.postings.length === 0 && canDoc(meta, SALES_ORDER, "delete");
  const blockedReason = dirty ? t("record.save_first") : undefined;

  const headerActions = (
    <>
      {workflow.slice(0, 2).map((action) => (
        <Button
          key={action.command.name}
          label={action.label}
          variant="secondary"
          isDisabled={dirty}
          tooltip={blockedReason}
          isLoading={running === action.command.name}
          onClick={() => void run(action)}
        />
      ))}
      {doc ? (
        <DropdownMenu
          alignment="end"
          hasChevron={false}
          renderTrigger={(trigger) => <IconButton {...trigger} label={t("spike.more")} tooltip={t("spike.more")} icon={<Icon icon="moreHorizontal" />} variant="ghost" />}
        >
          {workflow.slice(2).map((action) => (
            <DropdownMenuItem key={action.command.name} label={action.label} isDisabled={dirty} onClick={() => void run(action)} />
          ))}
          {canRun(meta, "document.render") ? <DropdownMenuItem label={t("print.print")} icon={Printer} href={paths.print(code, SALES_ORDER, doc.id)} /> : null}
          {destructive.map((action) => (
            <DropdownMenuItem key={action.command.name} label={action.label} variant="destructive" isDisabled={dirty} onClick={() => setConfirming(action)} />
          ))}
          {canDelete ? <DropdownMenuItem label={t("action.delete")} icon={Trash2} variant="destructive" onClick={() => setConfirming("delete")} /> : null}
        </DropdownMenu>
      ) : null}
      {canWrite ? (
        <Button
          label={isNew ? t("record.create") : t("action.save")}
          variant="primary"
          isLoading={saving}
          isDisabled={!isNew && !dirty}
          tooltip={t("record.save_hint")}
          onClick={() => void save()}
        />
      ) : null}
    </>
  );

  const facts = doc
    ? [
        <Status key="status" state={state} />,
        dirty ? <Text key="dirty" weight="semibold">{t("record.unsaved")}</Text> : null,
        typeof summary.customer_name === "string" ? <Text key="customer">{summary.customer_name}</Text> : null,
        <Text key="date">{formatCalendarDate(doc.doc_date)}</Text>,
        <Text key="by" color="secondary">
          {t("spike.created_by", { name: doc.created_by_name ?? "", at: formatTimestamp(doc.created_at) })}
        </Text>,
        previewing ? <Spinner key="previewing" size="sm" aria-label={t("fresh.working_out")} /> : null,
      ]
    : [<Status key="status" state={state} />, previewing ? <Spinner key="previewing" size="sm" aria-label={t("fresh.working_out")} /> : null];

  const lineProblems = problems.lines;
  const editable = canWrite;

  const details = (
    <Section>
      <VStack gap={4}>
        <Heading level={2}>{t("fresh.details")}</Heading>
        <Grid columns={{ minWidth: 240, max: 3 }} gap={4}>
          <RecordLookup
            entity="customer"
            label={field("customer_id")?.label ?? "customer_id"}
            value={form.header.customer_id}
            onChange={(value) =>
              setForm((previous) => ({
                ...previous,
                // Addresses belong to the customer: a new customer starts from its defaults.
                header: { ...previous.header, customer_id: value, billing_address_id: "", shipping_address_id: "" },
              }))
            }
            companyId={companyId}
            isRequired
            isDisabled={!editable}
            error={problems.header.customer_id}
          />
          <DateInput
            label={headerLabel("doc_date", t("document.date"))}
            value={form.docDate as ISODateString}
            onChange={(next) => setForm((previous) => ({ ...previous, docDate: next ?? "" }))}
            isRequired
            isDisabled={!editable}
            format="date"
            width="100%"
            status={problems.header.doc_date ? { type: "error", message: problems.header.doc_date } : undefined}
          />
          <RecordLookup
            entity="payment_terms"
            label={field("payment_terms_id")?.label ?? "payment_terms_id"}
            value={form.header.payment_terms_id}
            onChange={(value) => setHeader("payment_terms_id", value)}
            companyId={companyId}
            isDisabled={!editable}
            error={problems.header.payment_terms_id}
          />
          {(["billing_address_id", "shipping_address_id"] as const).map((name) => {
            const meta = field(name);
            return meta ? (
              <AddressSelector
                key={name}
                field={meta}
                kind={name === "billing_address_id" ? "billing" : "shipping"}
                customerId={form.header.customer_id}
                value={form.header[name]}
                isDisabled={!editable}
                error={problems.header[name]}
                onChange={(value) => setHeader(name, value)}
              />
            ) : null;
          })}
          <RecordLookup
            entity="currency"
            keyField="code"
            label={field("currency")?.label ?? "currency"}
            value={form.header.currency}
            onChange={(value) => setHeader("currency", value)}
            companyId={companyId}
            isDisabled={!editable}
            error={problems.header.currency}
          />
          <GridSpan columns="full">
            <TextInput
              label={headerLabel("memo", t("document.memo"))}
              value={form.memo}
              onChange={(next) => setForm((previous) => ({ ...previous, memo: next }))}
              isDisabled={!editable}
              width="100%"
              status={problems.header.memo ? { type: "error", message: problems.header.memo } : undefined}
            />
          </GridSpan>
        </Grid>
      </VStack>
    </Section>
  );

  const content =
    !isNew && !doc ? (
      <VStack gap={4}>
        <Skeleton height={180} />
        <Skeleton height={240} index={1} />
      </VStack>
    ) : tab === "history" && doc ? (
      <Section>
        <History id={doc.id} />
      </Section>
    ) : (
      <VStack gap={4}>
        {locked && state ? <Banner status="info" title={t("document.locked", { status: state.label })} /> : null}
        {stale ? (
          <Banner
            status="warning"
            title={t("conflict.title")}
            endContent={<Button label={t("conflict.take_theirs")} size="sm" onClick={loaded.reload} />}
          />
        ) : null}
        {formError ? <Banner status="error" title={t("record.save_failed")} description={formError} /> : null}
        {details}
        {lines ? (
          <Section>
            <VStack gap={4}>
              <HStack gap={2} vAlign="center">
                <Heading level={2}>{t("fresh.lines")}</Heading>
                <Text color="secondary">{t("fresh.line_count", { count: form.lines.length })}</Text>
              </HStack>
              <OrderLines
                collection={lines}
                lines={form.lines}
                nets={nets ?? form.lines.map((line) => (typeof line.stored.net_amount === "string" ? line.stored.net_amount : null))}
                editable={editable}
                companyId={companyId}
                decimals={decimals}
                problems={lineProblems}
                focusKey={focusKey}
                onChange={setLine}
                onAdd={(fromKeyboard) => {
                  const line = blankLine();
                  setForm((previous) => ({ ...previous, lines: [...previous.lines, line] }));
                  if (fromKeyboard) setFocusKey(line.key);
                }}
                onRemove={(key) => setForm((previous) => ({ ...previous, lines: previous.lines.filter((line) => line.key !== key) }))}
              />
              <Totals docType={docType} summary={{ currency, ...summary }} decimals={decimals} />
            </VStack>
          </Section>
        ) : null}
      </VStack>
    );

  const confirmAction = confirming && confirming !== "delete" ? confirming : null;

  return (
    <>
      <Layout
        height="fill"
        contentWidth={1000}
        defaultHasDividers
        header={
          <PageHeader
            back={{ label: docType.plural, href: paths.docList(code, SALES_ORDER) }}
            title={title}
            facts={facts}
            actions={headerActions}
            tab={tab}
            onTab={(next) => url.set({ tab: next === "order" ? null : next }, { replace: true })}
            showTabs={!isNew}
            isNarrow={isNarrow}
          />
        }
        content={<LayoutContent role="main">{content}</LayoutContent>}
      />
      <AlertDialog
        isOpen={blocker.state === "blocked"}
        onOpenChange={(open) => {
          if (!open && blocker.state === "blocked") blocker.reset();
        }}
        title={t("unsaved.title")}
        description={t("unsaved.description")}
        actionLabel={t("unsaved.leave")}
        cancelLabel={t("unsaved.stay")}
        onAction={() => blocker.state === "blocked" && blocker.proceed()}
      />
      <AlertDialog
        isOpen={confirmAction !== null}
        onOpenChange={(open) => !open && setConfirming(null)}
        title={t("fresh.confirm_title", { action: confirmAction?.label ?? "", subject: title })}
        description={t("fresh.confirm_description", { action: confirmAction?.label ?? "" })}
        actionLabel={confirmAction?.label ?? ""}
        isActionLoading={confirmAction !== null && running === confirmAction.command.name}
        onAction={() => confirmAction && void run(confirmAction)}
      />
      <AlertDialog
        isOpen={confirming === "delete"}
        onOpenChange={(open) => !open && setConfirming(null)}
        title={t("delete.title", { label: title })}
        description={t("delete.description")}
        actionLabel={t("action.delete")}
        isActionLoading={running === "delete"}
        onAction={() => void remove()}
      />
    </>
  );
}
