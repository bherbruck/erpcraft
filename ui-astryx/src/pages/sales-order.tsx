import { Badge } from "@astryxdesign/core/Badge";
import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Card } from "@astryxdesign/core/Card";
import { Center } from "@astryxdesign/core/Center";
import { DateInput } from "@astryxdesign/core/DateInput";
import type { ISODateString } from "@astryxdesign/core/utils";
import { Heading } from "@astryxdesign/core/Heading";
import { HStack } from "@astryxdesign/core/HStack";
import { Icon } from "@astryxdesign/core/Icon";
import { IconButton } from "@astryxdesign/core/IconButton";
import { Layout, LayoutContent, LayoutHeader } from "@astryxdesign/core/Layout";
import { Spinner } from "@astryxdesign/core/Spinner";
import {
  Table,
  TableBody,
  TableCell,
  TableFooter,
  TableHeader,
  TableHeaderCell,
  TableRow,
} from "@astryxdesign/core/Table";
import { Text } from "@astryxdesign/core/Text";
import { TextInput } from "@astryxdesign/core/TextInput";
import { VStack } from "@astryxdesign/core/VStack";
import { ArrowLeft, Plus, Trash2 } from "lucide-react";
import { type KeyboardEvent, useCallback, useEffect, useRef, useState } from "react";
import { useBlocker, useNavigate, useParams } from "react-router";
import { ApiError, api } from "@/api/client";
import type { DocTypeMeta, DocumentRecord, Json, JsonObject } from "@/api/types";
import { Amount } from "@/components/amount";
import { Lookup } from "@/components/lookup";
import { StatusBadge } from "@/components/status-badge";
import { t } from "@/i18n/t";
import { todayCalendarDate } from "@/lib/dates";
import { isDecimalDraft, trimDecimal } from "@/lib/decimal";
import { docType } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useSession } from "@/session";

// ------------------------------------------------------------------ model

/** The fields of a line a person edits. Everything else on a line is the server's. */
const EDITABLE = ["item_id", "warehouse_id", "charge_id", "account_id", "description", "qty", "unit_price", "discount_percent"] as const;
type EditableField = (typeof EDITABLE)[number];

interface Line {
  /** Stable React key; the server id once the line is saved. */
  key: string;
  values: Record<EditableField, string>;
  /** The line as the server last returned it (id, net_amount, delivered...). */
  server: JsonObject;
}

interface Draft {
  doc_date: string;
  memo: string;
  customer_id: string | null;
  currency: string | null;
  payment_terms_id: string | null;
  lines: Line[];
}

interface FieldError {
  field: string | null;
  line: number | null;
  message: string;
}

let lineCounter = 0;
const newKey = () => `new-${++lineCounter}`;

function text(value: Json | undefined): string {
  if (value === null || value === undefined) return "";
  return typeof value === "string" ? value : String(value);
}

function emptyLine(): Line {
  const values = Object.fromEntries(EDITABLE.map((f) => [f, ""])) as Record<EditableField, string>;
  return { key: newKey(), values, server: {} };
}

function lineFromServer(raw: Json): Line {
  const server = (typeof raw === "object" && raw !== null && !Array.isArray(raw) ? raw : {}) as JsonObject;
  const values = Object.fromEntries(
    EDITABLE.map((f) => {
      const v = text(server[f]);
      // Show "12" rather than "12.000000"; the value is still a decimal string.
      return [f, f === "qty" || f === "unit_price" || f === "discount_percent" ? (v ? trimDecimal(v) : "") : v];
    }),
  ) as Record<EditableField, string>;
  return { key: text(server.id) || newKey(), values, server };
}

function draftFromDocument(doc: DocumentRecord): Draft {
  const body = doc.body;
  const lines = Array.isArray(body.lines) ? body.lines.map(lineFromServer) : [];
  return {
    doc_date: doc.doc_date,
    memo: doc.memo ?? "",
    customer_id: text(body.customer_id) || null,
    currency: text(body.currency) || null,
    payment_terms_id: text(body.payment_terms_id) || null,
    lines: lines.length ? lines : [emptyLine()],
  };
}

function newDraft(): Draft {
  return {
    doc_date: todayCalendarDate(),
    memo: "",
    customer_id: null,
    currency: null,
    payment_terms_id: null,
    lines: [emptyLine()],
  };
}

function isBlank(line: Line): boolean {
  return EDITABLE.every((f) => line.values[f] === "");
}

/** The body the server receives. Blank lines are left out; empty values are null. */
function bodyOf(draft: Draft): JsonObject {
  return {
    customer_id: draft.customer_id,
    currency: draft.currency,
    payment_terms_id: draft.payment_terms_id,
    lines: draft.lines
      .filter((line) => !isBlank(line))
      .map((line) => {
        const out: JsonObject = {};
        if (line.server.id) out.id = line.server.id;
        // Fields this screen does not edit are sent back as they were.
        if (line.server.discount_amount !== undefined) out.discount_amount = line.server.discount_amount;
        for (const f of EDITABLE) out[f] = line.values[f] === "" ? null : line.values[f];
        return out;
      }),
  };
}

/** Which line the user sees as line n (1-based), counting only lines that are sent. */
function sentLineIndexes(draft: Draft): number[] {
  const indexes: number[] = [];
  draft.lines.forEach((line, i) => {
    if (!isBlank(line)) indexes.push(i);
  });
  return indexes;
}

/** What a save would send, to tell whether anything changed since the last save. */
function serialize(draft: Draft): string {
  return JSON.stringify({ ...bodyOf(draft), doc_date: draft.doc_date, memo: draft.memo });
}

function newIdempotencyKey(): string {
  return crypto.randomUUID();
}

// ------------------------------------------------------------------ page

export function SalesOrderPage() {
  const { company: code = "", id } = useParams();
  const { meta, companies } = useSession();
  const company = companies.find((c) => c.code === code);
  const type = docType(meta, "sales_order");
  const navigate = useNavigate();

  const [doc, setDoc] = useState<DocumentRecord | null>(null);
  const [initial] = useState(() => (id ? null : newDraft()));
  const [draft, setDraft] = useState<Draft | null>(initial);
  const [saved, setSaved] = useState<string>(initial ? serialize(initial) : "");
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [errors, setErrors] = useState<FieldError[]>([]);
  const [formError, setFormError] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const [preview, setPreview] = useState<DocumentRecord | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const idempotency = useRef(newIdempotencyKey());
  const tableRef = useRef<HTMLDivElement>(null);

  const load = useCallback(async () => {
    if (!id) return;
    try {
      const record = await api.run("document.get", { doc_type: "sales_order", id });
      const next = draftFromDocument(record);
      setDoc(record);
      setDraft(next);
      setSaved(serialize(next));
      setPreview(record);
      setStale(false);
      setLoadError(null);
    } catch (cause) {
      setLoadError(cause instanceof Error ? cause.message : String(cause));
    }
  }, [id]);

  useEffect(() => {
    void load();
  }, [load]);

  const current = draft ? serialize(draft) : saved;
  const hasChanges = draft !== null && current !== saved;
  const locked = doc?.locked ?? false;

  // Leaving with unsaved changes asks first, in the app and when closing the tab.
  const blocker = useBlocker(({ currentLocation, nextLocation }) => hasChanges && !saving && currentLocation.pathname !== nextLocation.pathname);
  useEffect(() => {
    if (blocker.state !== "blocked") return;
    if (window.confirm(t("doc.leave"))) blocker.proceed();
    else blocker.reset();
  }, [blocker]);
  useEffect(() => {
    if (!hasChanges) return;
    const handler = (event: BeforeUnloadEvent) => event.preventDefault();
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  }, [hasChanges]);

  // The server works out the totals: a preview of the same command, nothing kept.
  useEffect(() => {
    if (!draft || !company || locked || !draft.customer_id) return;
    const controller = new AbortController();
    const timer = setTimeout(async () => {
      setPreviewing(true);
      try {
        const input: JsonObject = id && doc
          ? { doc_type: "sales_order", id, version: doc.version, body: bodyOf(draft), doc_date: draft.doc_date }
          : { doc_type: "sales_order", company_id: company.id, doc_date: draft.doc_date, body: bodyOf(draft) };
        const result = await api.preview(id ? "document.update" : "document.create", input, { signal: controller.signal });
        setPreview(result as unknown as DocumentRecord);
      } catch {
        // An incomplete line cannot be previewed yet; the last totals stay until it can.
      } finally {
        if (!controller.signal.aborted) setPreviewing(false);
      }
    }, 400);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [current, company, id, doc, locked]);

  const save = useCallback(async () => {
    if (!draft || !company || saving || locked) return;
    setSaving(true);
    setErrors([]);
    setFormError(null);
    try {
      const body = bodyOf(draft);
      const options = { idempotencyKey: idempotency.current };
      const record =
        id && doc
          ? await api.run(
              "document.update",
              { doc_type: "sales_order", id, version: doc.version, body, doc_date: draft.doc_date, memo: draft.memo },
              options,
            )
          : await api.run(
              "document.create",
              { doc_type: "sales_order", company_id: company.id, doc_date: draft.doc_date, memo: draft.memo || null, body },
              options,
            );
      idempotency.current = newIdempotencyKey();
      const next = draftFromDocument(record);
      setDoc(record);
      setDraft(next);
      setPreview(record);
      setSaved(serialize(next));
      if (!id) navigate(paths.salesOrder(code, record.id), { replace: true });
    } catch (cause) {
      // Everything typed stays. The error goes next to its field, in the server's words.
      if (cause instanceof ApiError) {
        if (cause.isStale) setStale(true);
        else if (cause.field || cause.line) {
          const sent = sentLineIndexes(draft);
          const line = cause.line !== null ? (sent[cause.line - 1] ?? null) : null;
          setErrors([{ field: cause.field, line, message: cause.message }]);
          setFormError(cause.message);
        } else setFormError(cause.message);
      } else setFormError(String(cause));
    } finally {
      setSaving(false);
    }
  }, [draft, company, saving, locked, id, doc, navigate, code]);

  // Ctrl+S saves from anywhere on the page.
  useEffect(() => {
    const handler = (event: globalThis.KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        void save();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [save]);

  const errorFor = (field: string, line: number | null = null) =>
    errors.find((e) => e.field === field && e.line === line)?.message ?? null;

  if (loadError) {
    return (
      <VStack padding={4}>
        <Banner status="error" title={loadError} />
      </VStack>
    );
  }
  if (!draft) {
    return (
      <Center height="100%">
        <Spinner label={t("common.loading")} />
      </Center>
    );
  }

  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => setDraft((d) => (d ? { ...d, [key]: value } : d));
  const setLine = (index: number, field: EditableField, value: string) =>
    setDraft((d) =>
      d ? { ...d, lines: d.lines.map((l, i) => (i === index ? { ...l, values: { ...l.values, [field]: value } } : l)) } : d,
    );
  const addLine = () => {
    setDraft((d) => (d ? { ...d, lines: [...d.lines, emptyLine()] } : d));
    // Focus the new line's first field once it is rendered.
    requestAnimationFrame(() => {
      const rows = tableRef.current?.querySelectorAll<HTMLElement>("[data-line-first] input");
      rows?.[rows.length - 1]?.focus();
    });
  };
  const removeLine = (index: number) =>
    setDraft((d) => (d ? { ...d, lines: d.lines.length > 1 ? d.lines.filter((_, i) => i !== index) : [emptyLine()] } : d));

  const onLineKey = (index: number) => (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter" && index === draft.lines.length - 1 && !locked) {
      event.preventDefault();
      addLine();
    }
  };

  const title = doc?.number ?? (id ? t("list.draft") : t("doc.new"));
  const currency = draft.currency ?? text((preview?.summary as JsonObject | undefined)?.currency) ?? null;
  const summary = (preview?.summary ?? {}) as JsonObject;
  const previewLines = Array.isArray(preview?.body?.lines) ? (preview.body.lines as JsonObject[]) : [];
  const sent = sentLineIndexes(draft);
  const netOf = (index: number): string | null => {
    const position = sent.indexOf(index);
    if (position < 0) return null;
    return text(previewLines[position]?.net_amount) || null;
  };

  return (
    <Layout
      height="fill"
      header={<LayoutHeader hasDivider padding={3}>
        <HStack hAlign="between" vAlign="center" gap={3} wrap="wrap">
          <HStack gap={3} vAlign="center">
            <IconButton label={t("common.back")} icon={<Icon icon={ArrowLeft} />} variant="ghost" size="sm" onClick={() => navigate(-1)} />
            <VStack gap={0.5}>
              <Text type="supporting">{type?.label ?? "Sales order"}</Text>
              <HStack gap={2} vAlign="center">
                <Heading level={1}>{title}</Heading>
                {doc ? <StatusBadge type={type} status={doc.status} /> : <Badge label={t("list.draft")} />}
              </HStack>
            </VStack>
          </HStack>
          <HStack gap={2} vAlign="center">
            {hasChanges ? <Badge variant="warning" label={t("doc.unsaved")} /> : doc ? <Text type="supporting">{t("doc.saved")}</Text> : null}
            {!locked ? (
              <Button label={t("doc.save")} variant="primary" isLoading={saving} onClick={() => void save()} tooltip="Ctrl+S" />
            ) : null}
          </HStack>
        </HStack>
      </LayoutHeader>}
      content={<LayoutContent padding={4}>
        <VStack gap={4}>
          {locked && doc ? (
            <Banner status="info" title={t("doc.locked", { status: doc.status })} />
          ) : null}
          {stale ? (
            <Banner
              status="warning"
              title={t("doc.stale")}
              endContent={<Button label={t("doc.reload")} size="sm" onClick={() => void load()} />}
            />
          ) : null}
          {formError ? <Banner status="error" title={t("doc.saveFailed")} description={formError} /> : null}

          <Card padding={4}>
            <div className="header-grid">
              <Lookup
                label={fieldLabel(type, "customer_id")}
                entity="customer"
                value={draft.customer_id}
                onChange={(v) => set("customer_id", v)}
                recordHref={(cid) => paths.customer(code, cid)}
                isRequired
                isDisabled={locked}
                error={errorFor("customer_id")}
              />
              <DateInput
                label={headerLabel(meta.document_header, "doc_date")}
                value={draft.doc_date as ISODateString}
                onChange={(v) => set("doc_date", v ?? "")}
                isRequired
                isDisabled={locked}
                format="date"
                status={errorFor("doc_date") ? { type: "error", message: errorFor("doc_date") ?? "" } : undefined}
              />
              <Lookup
                label={fieldLabel(type, "currency")}
                entity="currency"
                keyField="code"
                value={draft.currency}
                onChange={(v) => set("currency", v)}
                isDisabled={locked}
                error={errorFor("currency")}
              />
              <Lookup
                label={fieldLabel(type, "payment_terms_id")}
                entity="payment_terms"
                value={draft.payment_terms_id}
                onChange={(v) => set("payment_terms_id", v)}
                isDisabled={locked}
                error={errorFor("payment_terms_id")}
              />
              <div className="span-2">
                <TextInput
                  label={headerLabel(meta.document_header, "memo")}
                  value={draft.memo}
                  onChange={(v) => set("memo", v)}
                  isDisabled={locked}
                  width="100%"
                />
              </div>
            </div>
          </Card>

          <VStack gap={2}>
            <HStack hAlign="between" vAlign="center">
              <Heading level={2}>{t("doc.lines")}</Heading>
              {previewing ? <Spinner size="sm" aria-label={t("doc.previewing")} /> : null}
            </HStack>
            <div ref={tableRef}>
              <Table density="compact" dividers="rows" className="lines-table">
                <colgroup>
                  <col className="col-no" />
                  <col className="col-ref" />
                  <col />
                  <col className="col-num" />
                  <col className="col-num" />
                  <col className="col-pct" />
                  <col className="col-net" />
                  <col className="col-act" />
                </colgroup>
                <TableHeader>
                  <TableRow>
                    <TableHeaderCell>{t("doc.lineNo")}</TableHeaderCell>
                    <TableHeaderCell>{t("doc.lineRef")}</TableHeaderCell>
                    <TableHeaderCell>{t("doc.lineDescription")}</TableHeaderCell>
                    <TableHeaderCell className="num">{lineLabel(type, "qty")}</TableHeaderCell>
                    <TableHeaderCell className="num">{lineLabel(type, "unit_price")}</TableHeaderCell>
                    <TableHeaderCell className="num">{lineLabel(type, "discount_percent")}</TableHeaderCell>
                    <TableHeaderCell className="num">{lineLabel(type, "net_amount")}</TableHeaderCell>
                    <TableHeaderCell>
                      <span className="visually-hidden">{t("doc.removeLine", { n: "" })}</span>
                    </TableHeaderCell>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {draft.lines.map((line, index) => {
                    const n = index + 1;
                    const kind: "item_id" | "charge_id" | "account_id" = line.values.charge_id
                      ? "charge_id"
                      : line.values.account_id
                        ? "account_id"
                        : "item_id";
                    const entityName = kind === "item_id" ? "item" : kind === "charge_id" ? "charge" : "account";
                    return (
                      <TableRow key={line.key}>
                        <TableCell className="num">
                          <Text type="supporting">{n}</Text>
                        </TableCell>
                        <TableCell>
                          <div data-line-first>
                            <Lookup
                              label={`${lineLabel(type, kind)}, ${n}`}
                              isLabelHidden
                              size="sm"
                              entity={entityName}
                              value={line.values[kind] || null}
                              onChange={(v) => setLine(index, kind, v ?? "")}
                              isDisabled={locked}
                              error={errorFor(kind, index)}
                            />
                          </div>
                        </TableCell>
                        <TableCell>
                          <TextInput
                            label={`${t("doc.lineDescription")}, ${n}`}
                            isLabelHidden
                            size="sm"
                            value={line.values.description}
                            onChange={(v) => setLine(index, "description", v)}
                            onKeyDown={onLineKey(index)}
                            isDisabled={locked}
                            status={errorFor("description", index) ? { type: "error", message: errorFor("description", index) ?? "" } : undefined}
                          />
                        </TableCell>
                        {(["qty", "unit_price", "discount_percent"] as const).map((field) => (
                          <TableCell key={field}>
                            <TextInput
                              label={`${lineLabel(type, field)}, ${n}`}
                              isLabelHidden
                              size="sm"
                              inputMode="decimal"
                              value={line.values[field]}
                              onChange={(v) => {
                                if (isDecimalDraft(v)) setLine(index, field, v);
                              }}
                              onKeyDown={onLineKey(index)}
                              isDisabled={locked}
                              isRequired={field !== "discount_percent"}
                              controlClassName="num"
                              status={errorFor(field, index) ? { type: "error", message: errorFor(field, index) ?? "" } : undefined}
                            />
                          </TableCell>
                        ))}
                        <TableCell className="num">
                          <Amount value={netOf(index)} />
                        </TableCell>
                        <TableCell>
                          {!locked ? (
                            <IconButton
                              label={t("doc.removeLine", { n })}
                              icon={<Icon icon={Trash2} />}
                              variant="ghost"
                              size="sm"
                              onClick={() => removeLine(index)}
                            />
                          ) : null}
                        </TableCell>
                      </TableRow>
                    );
                  })}
                </TableBody>
                <TableFooter>
                  <TableRow>
                    <TableCell colSpan={6} className="num">
                      <Text type="supporting">{t("doc.discount")}</Text>
                    </TableCell>
                    <TableCell className="num">
                      <Amount value={text(summary.total_discount) || null} />
                    </TableCell>
                    <TableCell />
                  </TableRow>
                  <TableRow>
                    <TableCell colSpan={6} className="num">
                      <Text type="supporting">{t("doc.charges")}</Text>
                    </TableCell>
                    <TableCell className="num">
                      <Amount value={text(summary.total_charges) || null} />
                    </TableCell>
                    <TableCell />
                  </TableRow>
                  <TableRow>
                    <TableCell colSpan={6} className="num">
                      <Text weight="semibold">{t("doc.subtotalNet")}</Text>
                    </TableCell>
                    <TableCell className="num">
                      <Amount value={text(summary.total_net) || null} currency={currency} weight="semibold" />
                    </TableCell>
                    <TableCell />
                  </TableRow>
                </TableFooter>
              </Table>
            </div>
            {!locked ? (
              <HStack>
                <Button label={t("doc.addLine")} icon={<Icon icon={Plus} />} variant="ghost" size="sm" onClick={addLine} />
              </HStack>
            ) : null}
          </VStack>
        </VStack>
      </LayoutContent>}
    />
  );
}

function fieldLabel(type: DocTypeMeta | undefined, name: string): string {
  return type?.fields.find((f) => f.name === name)?.label ?? name;
}

function lineLabel(type: DocTypeMeta | undefined, name: string): string {
  return type?.collections[0]?.fields.find((f) => f.name === name)?.label ?? name;
}

function headerLabel(header: { name: string; label: string }[], name: string): string {
  return header.find((f) => f.name === name)?.label ?? name;
}
