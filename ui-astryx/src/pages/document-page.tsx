import { AlertDialog } from "@astryxdesign/core/AlertDialog";
import { Badge } from "@astryxdesign/core/Badge";
import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Card } from "@astryxdesign/core/Card";
import { Center } from "@astryxdesign/core/Center";
import { DateInput } from "@astryxdesign/core/DateInput";
import { Heading } from "@astryxdesign/core/Heading";
import { HStack } from "@astryxdesign/core/HStack";
import { Icon } from "@astryxdesign/core/Icon";
import { IconButton } from "@astryxdesign/core/IconButton";
import { Link } from "@astryxdesign/core/Link";
import { MetadataList, MetadataListItem } from "@astryxdesign/core/MetadataList";
import { Selector } from "@astryxdesign/core/Selector";
import { Spinner } from "@astryxdesign/core/Spinner";
import { Tab, TabList } from "@astryxdesign/core/TabList";
import { Table, TableBody, TableCell, TableHeader, TableHeaderCell, TableRow } from "@astryxdesign/core/Table";
import { Text } from "@astryxdesign/core/Text";
import { TextInput } from "@astryxdesign/core/TextInput";
import type { ISODateString } from "@astryxdesign/core/utils";
import { VStack } from "@astryxdesign/core/VStack";
import { Plus, Trash2 } from "lucide-react";
import { type KeyboardEvent, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { ApiError, api } from "@/api/client";
import type { CollectionMeta, DocTypeMeta, DocumentRecord, FieldMeta, Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { ActionButtons, ActionDialog } from "@/components/actions";
import { useDecimalHeader } from "@/components/decimal-value";
import { FieldInput, type FieldValues, initialValues, toEditable } from "@/components/field-input";
import { FieldValue, isNumeric } from "@/components/field-value";
import { History } from "@/components/history";
import { Page } from "@/components/page";
import { Postings } from "@/components/postings";
import type { FieldContext } from "@/components/references";
import { StatusBadge } from "@/components/status-badge";
import { UnsavedDialog, useUnsavedGuard } from "@/components/unsaved-guard";
import { t } from "@/i18n/t";
import { todayCalendarDate } from "@/lib/dates";
import { canDoc, findDocType, recordActions, type ResourceAction } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

export function DocumentPage() {
  const { meta } = useApp();
  const { company } = useCompany();
  const params = useParams();
  const docType = findDocType(meta, params.docType);
  if (!docType || !canDoc(meta, docType.name, "read")) return <NotFoundPage />;
  if (!params.id && (!canDoc(meta, docType.name, "create") || !company)) return <NotFoundPage />;
  return <DocumentForm key={`${docType.name}:${params.id ?? "new"}`} docType={docType} id={params.id ?? null} />;
}

// ------------------------------------------------------------------ model (as ../ui)

interface Line {
  key: string;
  /** The line as the server returned it: sent back with the user's changes. */
  stored: JsonObject;
  values: FieldValues;
}

interface FormState {
  docDate: string;
  memo: string;
  stored: JsonObject;
  header: FieldValues;
  collections: Record<string, Line[]>;
}

interface Problems {
  header: Record<string, string>;
  lines: Record<string, Record<number, { line?: string; fields: Record<string, string> }>>;
}

const NO_PROBLEMS: Problems = { header: {}, lines: {} };

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const isBlank = (value: Json | undefined): boolean => value === undefined || value === null || value === "";

function pick(fields: FieldMeta[], source: JsonObject): FieldValues {
  const values: FieldValues = {};
  for (const field of fields) values[field.name] = toEditable(field, source[field.name]);
  return values;
}

/** Whether the server writes every field of a collection's lines. */
function serverLines(collection: CollectionMeta): boolean {
  return collection.fields.every((field) => field.readonly);
}

function newLine(collection: CollectionMeta): Line {
  return { key: newKey(), stored: {}, values: initialValues(collection.fields) };
}

function blankForm(docType: DocTypeMeta): FormState {
  const collections: Record<string, Line[]> = {};
  for (const collection of docType.collections)
    collections[collection.name] = serverLines(collection) ? [] : [newLine(collection)];
  return { docDate: todayCalendarDate(), memo: "", stored: {}, header: initialValues(docType.fields), collections };
}

function formFromDocument(docType: DocTypeMeta, doc: DocumentRecord): FormState {
  const collections: Record<string, Line[]> = {};
  const stored: JsonObject = { ...doc.body };
  for (const collection of docType.collections) {
    const lines = doc.body[collection.name];
    delete stored[collection.name];
    collections[collection.name] = (Array.isArray(lines) ? lines : []).filter(isObject).map((line) => ({
      key: typeof line.id === "string" ? line.id : newKey(),
      stored: line,
      values: pick(collection.fields, line),
    }));
  }
  return { docDate: doc.doc_date, memo: doc.memo ?? "", stored, header: pick(docType.fields, doc.body), collections };
}

function written(fields: FieldMeta[], stored: JsonObject, values: FieldValues): JsonObject {
  const out: JsonObject = { ...stored };
  for (const field of fields) {
    const value = values[field.name];
    if (isBlank(value)) {
      if (field.name in stored || !field.readonly) out[field.name] = null;
    } else if (value !== undefined) out[field.name] = value;
  }
  return out;
}

/** The body as the server takes it; a new line left untouched is not sent. */
function bodyOf(docType: DocTypeMeta, form: FormState): JsonObject {
  const body = written(docType.fields, form.stored, form.header);
  for (const collection of docType.collections) {
    const fresh = initialValues(collection.fields);
    body[collection.name] = (form.collections[collection.name] ?? [])
      .filter(
        (line) =>
          Object.keys(line.stored).length > 0 ||
          collection.fields.some((field) => !isBlank(line.values[field.name]) && line.values[field.name] !== fresh[field.name]),
      )
      .map((line) => written(collection.fields, line.stored, line.values));
  }
  return body;
}

function snapshot(docType: DocTypeMeta, form: FormState): string {
  return JSON.stringify({ doc_date: form.docDate, memo: form.memo, body: bodyOf(docType, form) });
}

/** The fields a line grid shows: not the line's id, not its stored number (that is the row number). */
function lineFields(collection: CollectionMeta): FieldMeta[] {
  return collection.fields.filter((field) => !(field.readonly && (field.type.kind === "uuid" || field.name === "line_no")));
}

function problemsOf(error: ApiError, docType: DocTypeMeta): Problems | null {
  if (error.collection && error.line !== null) {
    const entry = { line: error.field ? undefined : error.message, fields: error.field ? { [error.field]: error.message } : {} };
    return { header: {}, lines: { [error.collection]: { [error.line]: entry } } };
  }
  if (error.field && (error.field === "doc_date" || error.field === "memo" || docType.fields.some((f) => f.name === error.field)))
    return { header: { [error.field]: error.message }, lines: {} };
  return null;
}

// ------------------------------------------------------------------ line grid

function ColumnHead({ field }: { field: FieldMeta }) {
  const label = useDecimalHeader(field);
  return <TableHeaderCell className={isNumeric(field) ? "num" : undefined}>{label}</TableHeaderCell>;
}

/**
 * The lines of one collection. Astryx's Table in children mode with an Astryx input in
 * each cell: the spike's stand-in for the headless grid (packages/grid).
 */
function LineGrid({
  docType,
  collection,
  lines,
  header,
  editable,
  problems,
  computed,
  companyId,
  onChange,
}: {
  docType: DocTypeMeta;
  collection: CollectionMeta;
  lines: Line[];
  header: FieldValues;
  editable: boolean;
  problems: Problems["lines"][string] | undefined;
  /** The server's preview of the sent lines, for the values it works out (net). */
  computed?: JsonObject[];
  companyId: string | null;
  onChange: (lines: Line[]) => void;
}) {
  const gridRef = useRef<HTMLDivElement>(null);
  const kinds = docType.alternatives.find((alternative) => alternative.collection === collection.name)?.one_of;
  const kindFields = new Set((kinds ?? []).flatMap((kind) => kind.fields));
  const all = lineFields(collection);
  const firstKindField = all.find((field) => kindFields.has(field.name))?.name;
  // A read-only column nobody has a value in (delivered, while nothing is) says nothing yet.
  const visible = all.filter(
    (field) =>
      field.name === firstKindField ||
      (!kindFields.has(field.name) && !(field.readonly && lines.length > 0 && lines.every((line) => isBlank(line.values[field.name])))),
  );
  const [chosenKind, setChosenKind] = useState<Record<string, number>>({});
  const kindOf = (line: Line): number => {
    const filled = (kinds ?? []).findIndex((kind) => kind.fields.some((name) => !isBlank(line.values[name])));
    return filled >= 0 ? filled : (chosenKind[line.key] ?? 0);
  };
  const contextFields = [...docType.fields, ...collection.fields];
  // Which preview line each line is: blank new lines are not sent, so they have none.
  const fresh = initialValues(collection.fields);
  const sentIndex = new Map<string, number>();
  for (const line of lines) {
    const sent =
      Object.keys(line.stored).length > 0 ||
      collection.fields.some((field) => !isBlank(line.values[field.name]) && line.values[field.name] !== fresh[field.name]);
    if (sent) sentIndex.set(line.key, sentIndex.size);
  }

  function setValue(line: Line, name: string, value: Json | undefined): void {
    onChange(lines.map((other) => (other.key === line.key ? { ...other, values: { ...other.values, [name]: value } } : other)));
  }

  function add(): void {
    onChange([...lines, newLine(collection)]);
    requestAnimationFrame(() => {
      const rows = gridRef.current?.querySelectorAll<HTMLElement>("tbody tr");
      rows?.[rows.length - 1]?.querySelector<HTMLElement>("input, button[role=combobox], [role=combobox]")?.focus();
    });
  }

  function changeKind(line: Line, next: number): void {
    setChosenKind((previous) => ({ ...previous, [line.key]: next }));
    const cleared: FieldValues = { ...line.values };
    (kinds ?? []).forEach((kind, index) => {
      if (index !== next) for (const name of kind.fields) cleared[name] = null;
    });
    onChange(lines.map((other) => (other.key === line.key ? { ...other, values: cleared } : other)));
  }

  const onKey = (isLast: boolean) => (event: KeyboardEvent<HTMLElement>) => {
    // Enter on the last line adds a line, so a document can be typed in without the mouse.
    if (event.key === "Enter" && isLast && editable && (event.target as HTMLElement).tagName === "INPUT") {
      const expanded = (event.target as HTMLElement).getAttribute("aria-expanded") === "true";
      if (expanded) return;
      event.preventDefault();
      add();
    }
  };

  const cell = (line: Line, field: FieldMeta, number: number, isLast: boolean) => {
    const context: FieldContext = { header, line: line.values, fields: contextFields };
    const error = problems?.[number]?.fields[field.name];
    if (!editable || field.readonly) {
      const index = sentIndex.get(line.key);
      const worked = field.readonly && computed && index !== undefined ? computed[index]?.[field.name] : undefined;
      return (
        <span className={isNumeric(field) ? "cell-value num" : "cell-value"}>
          <FieldValue field={field} value={worked ?? line.values[field.name]} context={context} bare={isNumeric(field)} linkify />
        </span>
      );
    }
    return (
      <FieldInput
        field={field}
        label={`${field.label}, ${t("lines.line_n", { line: number })}`}
        isLabelHidden
        size="sm"
        value={line.values[field.name]}
        error={error}
        companyId={companyId}
        context={context}
        onKeyDown={onKey(isLast)}
        onChange={(value) => setValue(line, field.name, value)}
      />
    );
  };

  return (
    <VStack gap={2}>
      <Heading level={2}>{collection.label}</Heading>
      <div ref={gridRef} className="lines-scroll">
        <Table density="compact" dividers="rows" className="lines-table">
          <colgroup>
            <col className="col-no" />
            {visible.map((field) =>
              field.name === firstKindField && kinds ? (
                <col key="kind" className="col-kind" />
              ) : (
                <col
                  key={field.name}
                  className={isNumeric(field) ? "col-num" : field.type.kind === "text" ? "col-text" : field.type.kind === "date" ? "col-date" : "col-ref"}
                />
              ),
            )}
            {editable ? <col className="col-act" /> : null}
          </colgroup>
          <TableHeader>
            <TableRow>
              <TableHeaderCell className="num">{t("lines.number")}</TableHeaderCell>
              {visible.map((field) =>
                field.name === firstKindField && kinds ? (
                  <TableHeaderCell key="kind">{kinds.map((kind) => kind.label).join(t("lines.kind_or"))}</TableHeaderCell>
                ) : (
                  <ColumnHead key={field.name} field={field} />
                ),
              )}
              {editable ? (
                <TableHeaderCell>
                  <span className="visually-hidden">{t("lines.remove_hint")}</span>
                </TableHeaderCell>
              ) : null}
            </TableRow>
          </TableHeader>
          <TableBody>
            {lines.map((line, index) => {
              const number = index + 1;
              const isLast = index === lines.length - 1;
              const lineProblem = problems?.[number]?.line;
              return (
                <TableRow key={line.key}>
                  <TableCell className="num">
                    <Text type="supporting" hasTabularNumbers>
                      {number}
                    </Text>
                  </TableCell>
                  {visible.map((field) => {
                    if (field.name === firstKindField && kinds) {
                      const active = kindOf(line);
                      const kind = kinds[active];
                      return (
                        <TableCell key="kind">
                          <div className={editable && kinds.length > 1 ? "kind-cell" : undefined}>
                            {editable && kinds.length > 1 ? (
                              <Selector
                                label={t("lines.kind", { line: number })}
                                isLabelHidden
                                size="sm"
                                variant="ghost"
                                value={String(active)}
                                onChange={(next: string) => changeKind(line, Number(next))}
                                options={kinds.map((k, i) => ({ value: String(i), label: k.label }))}
                              />
                            ) : null}
                            <VStack gap={1}>
                              {(kind?.fields ?? []).map((name) => {
                                const kindField = collection.fields.find((f) => f.name === name);
                                return kindField ? <div key={name}>{cell(line, kindField, number, isLast)}</div> : null;
                              })}
                              {lineProblem ? <Text color="secondary">{lineProblem}</Text> : null}
                            </VStack>
                          </div>
                        </TableCell>
                      );
                    }
                    return <TableCell key={field.name}>{cell(line, field, number, isLast)}</TableCell>;
                  })}
                  {editable ? (
                    <TableCell>
                      <IconButton
                        label={t("lines.remove", { line: number })}
                        icon={<Icon icon={Trash2} />}
                        variant="ghost"
                        size="sm"
                        onClick={() => onChange(lines.filter((other) => other.key !== line.key))}
                      />
                    </TableCell>
                  ) : null}
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </div>
      {editable ? (
        <HStack>
          <Button label={t("lines.add")} icon={<Icon icon={Plus} />} variant="ghost" size="sm" onClick={add} />
        </HStack>
      ) : null}
    </VStack>
  );
}

// ------------------------------------------------------------------ totals

function TotalItem({ docType, field, summary }: { docType: DocTypeMeta; field: FieldMeta; summary: JsonObject }) {
  // Money in the company's currency names the currency in its label.
  const label = useDecimalHeader(field);
  return (
    <MetadataListItem label={label}>
      <span className="num-block" title={field.help ?? undefined}>
        {isBlank(summary[field.name]) ? (
          <Text color="secondary">{t("document.not_known_yet")}</Text>
        ) : (
          <FieldValue field={field} value={summary[field.name]} context={{ header: summary, fields: docType.summary }} />
        )}
      </span>
    </MetadataListItem>
  );
}

/** The figures the document type keeps in its summary, worked out by the server. */
function Totals({ docType, summary }: { docType: DocTypeMeta; summary: JsonObject }) {
  const figures = docType.summary.filter((field) => field.type.kind === "decimal" && !field.name.endsWith("_accounting"));
  if (figures.length === 0 || !figures.some((field) => field.name in summary)) return null;
  return (
    <div className="totals">
      <Card padding={3}>
        <MetadataList label={{ position: "start", width: "60%" }}>
          {figures.map((field) => (
            <TotalItem key={field.name} docType={docType} field={field} summary={summary} />
          ))}
        </MetadataList>
      </Card>
    </div>
  );
}

// ------------------------------------------------------------------ the page

/**
 * The page of any document type, generated from the registry: header fields, one line
 * grid per collection, the totals the server works out (live, by preview), the actions
 * of the workflow from the current state, postings, links and history.
 */
function DocumentForm({ docType, id }: { docType: DocTypeMeta; id: string | null }) {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const navigate = useNavigate();
  const url = useUrlState();
  const isNew = id === null;
  const tab = url.get("tab") === "history" && !isNew ? "history" : "document";

  const loaded = useAsync(id ? `${docType.name}|${id}` : null, (signal) =>
    api.run("document.get", { doc_type: docType.name, id: id ?? "" }, { signal }),
  );
  const [doc, setDoc] = useState<DocumentRecord | null>(null);
  const [form, setForm] = useState<FormState>(() => blankForm(docType));
  const [savedSnapshot, setSavedSnapshot] = useState(() => snapshot(docType, blankForm(docType)));
  const [problems, setProblems] = useState<Problems>(NO_PROBLEMS);
  const [formError, setFormError] = useState<string | null>(null);
  const [stale, setStale] = useState(false);
  const [saving, setSaving] = useState(false);
  const [preview, setPreview] = useState<JsonObject | null>(null);
  /** The lines as the server's preview worked them out (net amounts), by collection, in sent order. */
  const [previewLines, setPreviewLines] = useState<Record<string, JsonObject[]>>({});
  const [previewing, setPreviewing] = useState(false);
  const [action, setAction] = useState<ResourceAction | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);

  function adopt(next: DocumentRecord): void {
    const nextForm = formFromDocument(docType, next);
    setDoc(next);
    setForm(nextForm);
    setSavedSnapshot(snapshot(docType, nextForm));
    setPreview(isObject(next.summary) ? next.summary : null);
    setPreviewLines({});
    setStale(false);
  }

  // biome-ignore lint/correctness/useExhaustiveDependencies: adopt only what was loaded
  useEffect(() => {
    if (loaded.data) adopt(loaded.data);
  }, [loaded.data]);

  const locked = doc?.locked ?? false;
  const canWrite = isNew ? canDoc(meta, docType.name, "create") : !locked && canDoc(meta, docType.name, "update");
  const current = snapshot(docType, form);
  const dirty = canWrite && current !== savedSnapshot;
  const { blocker, allowNext } = useUnsavedGuard(dirty);
  const header: FieldValues = { ...form.header, company_id: doc?.company_id ?? company?.id ?? null };

  const keyRef = useRef<{ payload: string; key: string }>({ payload: "", key: newKey() });
  const keyFor = (payload: string): string => {
    if (keyRef.current.payload !== payload) keyRef.current = { payload, key: newKey() };
    return keyRef.current.key;
  };

  // The server works out the totals: a preview of the same save, nothing kept.
  // biome-ignore lint/correctness/useExhaustiveDependencies: the snapshot names the form
  useEffect(() => {
    if (!canWrite || !company || (!isNew && !doc) || current === savedSnapshot) return;
    const controller = new AbortController();
    const timer = setTimeout(async () => {
      setPreviewing(true);
      try {
        const body = bodyOf(docType, form);
        const result = doc
          ? await api.preview("document.update", { doc_type: docType.name, id: doc.id, version: doc.version, body, doc_date: form.docDate }, { signal: controller.signal })
          : await api.preview("document.create", { doc_type: docType.name, company_id: company.id, doc_date: form.docDate, body }, { signal: controller.signal });
        if (isObject(result) && isObject(result.summary)) setPreview(result.summary);
        if (isObject(result) && isObject(result.body)) {
          const body = result.body;
          const next: Record<string, JsonObject[]> = {};
          for (const collection of docType.collections) {
            const lines = body[collection.name];
            next[collection.name] = Array.isArray(lines) ? lines.filter(isObject) : [];
          }
          setPreviewLines(next);
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
    if (!canWrite || saving || !company || (!dirty && !isNew)) return;
    setSaving(true);
    setFormError(null);
    setProblems(NO_PROBLEMS);
    const body = bodyOf(docType, form);
    try {
      if (!doc) {
        const payload = { doc_type: docType.name, company_id: company.id, doc_date: form.docDate, memo: form.memo || null, body };
        const created = await api.run("document.create", payload, { idempotencyKey: keyFor(JSON.stringify(payload)) });
        adopt(created);
        allowNext();
        navigate(paths.doc(code, docType.name, created.id), { replace: true });
      } else {
        const payload = { doc_type: docType.name, id: doc.id, version: doc.version, body, doc_date: form.docDate, memo: form.memo };
        adopt(await api.run("document.update", payload, { idempotencyKey: keyFor(JSON.stringify(payload)) }));
      }
    } catch (cause) {
      const error = cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected"));
      if (error.isStale) setStale(true);
      else {
        // Everything typed stays; the error goes to its field or line, in the server's words.
        const placed = problemsOf(error, docType);
        if (placed) setProblems(placed);
        // An error placed at its field is said there once; the banner only points to it.
        setFormError(placed ? t("spike.see_marked_field") : error.message);
      }
    }
    setSaving(false);
  }

  const saveRef = useRef(save);
  saveRef.current = save;
  useEffect(() => {
    const onKey = (event: globalThis.KeyboardEvent): void => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        if (document.querySelector("dialog[open]")) return;
        void saveRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  async function remove(): Promise<void> {
    if (!doc) return;
    setDeleting(true);
    try {
      await api.run("document.delete", { doc_type: docType.name, id: doc.id });
      allowNext();
      navigate(paths.docList(code, docType.name), { replace: true });
    } catch (cause) {
      setConfirmDelete(false);
      setDeleting(false);
      setFormError(cause instanceof ApiError ? cause.message : String(cause));
    }
  }

  const actions = useMemo(() => (doc ? recordActions(meta, docType.name, doc.status) : []), [meta, docType.name, doc]);

  if (loaded.error && !doc) {
    return (
      <VStack padding={4}>
        <Banner status="error" title={t("record.load_failed")} description={loaded.error.message} />
      </VStack>
    );
  }
  if (!isNew && !doc) {
    return (
      <Center height="100%">
        <Spinner label={t("value.loading")} />
      </Center>
    );
  }

  const docHeader = (name: string, fallback: string) => meta.document_header.find((f) => f.name === name)?.label ?? fallback;
  const title = doc ? (doc.number ?? t("document.unnumbered")) : t("record.new", { label: docType.label });
  const editable = canWrite;
  const editableFields = docType.fields.filter((field) => !field.readonly && (isNew || !field.immutable));
  const readonlyFields = docType.fields.filter((field) => field.readonly || (!isNew && field.immutable));
  const summary = preview ?? (doc && isObject(doc.summary) ? doc.summary : {});
  const set = (change: Partial<FormState>) => setForm((previous) => ({ ...previous, ...change }));

  return (
    <Page
      back
      kicker={docType.label}
      title={title}
      badge={
        <HStack gap={2} vAlign="center">
          {doc ? <StatusBadge status={doc.status} docType={docType} /> : null}
          {dirty ? <Badge variant="warning" label={t("record.unsaved")} /> : null}
          {previewing ? <Spinner size="sm" aria-label={t("value.loading")} /> : null}
        </HStack>
      }
      actions={
        <>
          <ActionButtons actions={actions} disabledReason={dirty ? t("record.save_first") : null} onChoose={setAction} />
          {doc && !locked && doc.postings.length === 0 && canDoc(meta, docType.name, "delete") ? (
            <Button label={t("action.delete")} variant="ghost" onClick={() => setConfirmDelete(true)} />
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
      }
      toolbar={
        isNew ? null : (
          <TabList value={tab} onChange={(next) => url.set({ tab: next === "document" ? null : next }, { replace: true })} size="sm">
            <Tab value="document" label={t("document.tab")} />
            <Tab value="history" label={t("history.title")} />
          </TabList>
        )
      }
    >
      {tab === "history" && doc ? (
        <History resource={docType.name} id={doc.id} version={doc.version} />
      ) : (
        <VStack gap={5}>
          {locked && doc ? <Banner status="info" title={t("document.locked", { status: docType.states.find((s) => s.name === doc.status)?.label ?? doc.status })} /> : null}
          {stale ? (
            <Banner
              status="warning"
              title={t("conflict.title")}
              endContent={<Button label={t("conflict.take_theirs")} size="sm" onClick={loaded.reload} />}
            />
          ) : null}
          {formError ? <Banner status="error" title={t("record.save_failed")} description={formError} /> : null}

          <Card padding={4}>
            <VStack gap={4}>
              <div className="form-grid">
                <DateInput
                  label={docHeader("doc_date", t("document.date"))}
                  value={form.docDate as ISODateString}
                  onChange={(next) => set({ docDate: next ?? "" })}
                  isRequired
                  isDisabled={!editable}
                  format="date"
                  status={problems.header.doc_date ? { type: "error", message: problems.header.doc_date } : undefined}
                />
                {editableFields.map((field) => (
                  <FieldInput
                    key={field.name}
                    field={field}
                    value={form.header[field.name]}
                    disabled={!editable}
                    error={problems.header[field.name]}
                    companyId={doc?.company_id ?? company?.id}
                    context={{ header, fields: docType.fields }}
                    onChange={(value) => set({ header: { ...form.header, [field.name]: value } })}
                  />
                ))}
                <div className="span-2">
                  <TextInput
                    label={docHeader("memo", t("document.memo"))}
                    value={form.memo}
                    onChange={(next) => set({ memo: next })}
                    isDisabled={!editable}
                    width="100%"
                  />
                </div>
              </div>
              {readonlyFields.length > 0 || doc ? (
                <MetadataList columns="multi">
                  {readonlyFields.map((field) => (
                    <MetadataListItem key={field.name} label={field.label}>
                      <FieldValue field={field} value={form.header[field.name]} linkify context={{ header, fields: docType.fields }} />
                    </MetadataListItem>
                  ))}
                  {doc ? (
                    <MetadataListItem label={t("document.created_by_column")}>
                      {doc.created_by_name ?? doc.created_by}
                    </MetadataListItem>
                  ) : null}
                </MetadataList>
              ) : null}
            </VStack>
          </Card>

          {docType.collections.map((collection) => (
            <LineGrid
              key={collection.name}
              docType={docType}
              collection={collection}
              lines={form.collections[collection.name] ?? []}
              header={header}
              editable={editable && !serverLines(collection)}
              problems={problems.lines[collection.name]}
              computed={dirty ? previewLines[collection.name] : undefined}
              companyId={doc?.company_id ?? company?.id ?? null}
              onChange={(lines) => set({ collections: { ...form.collections, [collection.name]: lines } })}
            />
          ))}

          <Totals docType={docType} summary={summary} />

          {doc && doc.links.length > 0 ? (
            <VStack gap={2}>
              <Heading level={2}>{t("document.links")}</Heading>
              <MetadataList>
                {doc.links.map((link) => {
                  const linkedType = findDocType(meta, link.doc_type);
                  return (
                    <MetadataListItem
                      key={`${link.relation}.${link.id}`}
                      label={link.relation === "source" ? t("document.link_source") : t("document.link_follow_up")}
                    >
                      <HStack gap={2} vAlign="center">
                        <Link href={paths.doc(code, link.doc_type, link.id)}>
                          {`${linkedType?.label ?? link.doc_type} ${link.number ?? t("document.unnumbered")}`}
                        </Link>
                        <StatusBadge status={link.status} docType={linkedType} />
                      </HStack>
                    </MetadataListItem>
                  );
                })}
              </MetadataList>
            </VStack>
          ) : null}

          {doc && doc.postings.length > 0 ? <Postings postings={doc.postings} currency={company?.base_currency ?? ""} /> : null}
        </VStack>
      )}
      <UnsavedDialog blocker={blocker} />
      <AlertDialog
        isOpen={confirmDelete}
        onOpenChange={(open) => setConfirmDelete(open)}
        title={t("delete.title", { label: title })}
        description={t("delete.description")}
        actionLabel={t("action.delete")}
        isActionLoading={deleting}
        onAction={() => void remove()}
      />
      {action && doc ? (
        <ActionDialog
          action={action}
          id={doc.id}
          subject={`${docType.label} ${title}`}
          currency={company?.base_currency ?? ""}
          onClose={() => setAction(null)}
          onDone={(result) => {
            setAction(null);
            const created = isObject(result) && typeof result.id === "string" && result.id !== doc.id ? result.id : null;
            if (action.creates && created) navigate(paths.doc(code, action.creates, created));
            else loaded.reload();
          }}
        />
      ) : null}
    </Page>
  );
}

export default DocumentPage;
