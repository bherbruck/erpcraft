import { Plus, Printer, Trash2, X } from "lucide-react";
import { type KeyboardEvent as ReactKeyboardEvent, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { ApiError, api } from "@/api/client";
import type { CollectionMeta, DocTypeMeta, DocumentRecord, FieldMeta, Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { useOfferPageActions } from "@/app/page-actions";
import { ApprovalNotice } from "@/components/approval-notice";
import { BANK_STATEMENT, BankMatching } from "@/components/bank-matching";
import { ActionButtons, ActionDialog, useFollowOn } from "@/components/command-action";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { DateInput } from "@/components/date-input";
import { plainSource, useDecimalHeader } from "@/components/decimal-value";
import { FieldInput, type FieldValues, initialValues, toEditable } from "@/components/field-input";
import { FieldValue, isNumeric } from "@/components/field-value";
import { History } from "@/components/history";
import { PageHeader, Section, Tabs } from "@/components/page";
import { Postings } from "@/components/postings";
import type { FieldContext } from "@/components/references";
import { StatusBadge } from "@/components/status-badge";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { UnsavedDialog, useUnsavedGuard } from "@/components/unsaved-guard";
import { t } from "@/i18n/t";
import { cn } from "@/lib/cn";
import { formatCalendarDate, formatTimestamp, todayCalendarDate } from "@/lib/dates";
import { forgetDocuments, rememberDocument } from "@/lib/doc-labels";
import { canDoc, canRun, findDocType, recordActions } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { EinvoiceExportButton } from "./einvoice-parts";
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

// ------------------------------------------------------------------ form state

interface Line {
  /** Names the line on this screen only; the server never sees it. */
  key: string;
  /** The line as the server returned it, sent back with the user's changes on top. */
  stored: JsonObject;
  values: FieldValues;
}

interface FormState {
  docDate: string;
  memo: string;
  /** The body as the server returned it, besides the collections. */
  stored: JsonObject;
  header: FieldValues;
  collections: Record<string, Line[]>;
}

/** Where the server says a failed save went wrong. */
interface Problems {
  /** By header field, including `doc_date` and `memo`. */
  header: Record<string, string>;
  /** By collection, then line (from 1): a message for the line, and one per field of it. */
  lines: Record<string, Record<number, { line?: string; fields: Record<string, string> }>>;
}

const NO_PROBLEMS: Problems = { header: {}, lines: {} };

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function pick(fields: FieldMeta[], source: JsonObject): FieldValues {
  const values: FieldValues = {};
  for (const field of fields) values[field.name] = toEditable(field, source[field.name]);
  return values;
}

/** Whether the server writes every field of a collection's lines: they are its proposal, not typed. */
function serverLines(collection: CollectionMeta): boolean {
  return collection.fields.every((field) => field.readonly);
}

function newLine(collection: CollectionMeta): Line {
  return { key: newKey(), stored: {}, values: initialValues(collection.fields) };
}

function blankForm(docType: DocTypeMeta): FormState {
  const collections: Record<string, Line[]> = {};
  // Lines the server writes in full (a depreciation run's) start empty; others start with one to type in.
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
  return {
    docDate: doc.doc_date,
    memo: doc.memo ?? "",
    stored,
    header: pick(docType.fields, doc.body),
    collections,
  };
}

const isBlank = (value: Json | undefined): boolean => value === undefined || value === null || value === "";

/** The values of a part of the document as the server takes them: an empty input is null. */
function written(fields: FieldMeta[], stored: JsonObject, values: FieldValues): JsonObject {
  const out: JsonObject = { ...stored };
  for (const field of fields) {
    const value = values[field.name];
    if (isBlank(value)) {
      if (field.name in stored || !field.readonly) out[field.name] = null;
    } else if (value !== undefined) {
      out[field.name] = value;
    }
  }
  return out;
}

/**
 * The body as the server takes it: what it returned, with the user's changes.
 * A document accepts what it returns, so lines go back with their ids and
 * computed fields. A new line the user left untouched is not sent.
 */
function bodyOf(docType: DocTypeMeta, form: FormState): JsonObject {
  const body = written(docType.fields, form.stored, form.header);
  for (const collection of docType.collections) {
    const fresh = initialValues(collection.fields);
    body[collection.name] = (form.collections[collection.name] ?? [])
      .filter(
        (line) =>
          Object.keys(line.stored).length > 0 ||
          collection.fields.some(
            (field) => !isBlank(line.values[field.name]) && line.values[field.name] !== fresh[field.name],
          ),
      )
      .map((line) => written(collection.fields, line.stored, line.values));
  }
  return body;
}

function snapshot(docType: DocTypeMeta, form: FormState): string {
  return JSON.stringify({ doc_date: form.docDate, memo: form.memo, body: bodyOf(docType, form) });
}

/** The fields a line grid shows: not the line's id, and not its stored number, which is the row number. */
function lineFields(collection: CollectionMeta): FieldMeta[] {
  return collection.fields.filter(
    (field) => !(field.readonly && (field.type.kind === "uuid" || field.name === "line_no")),
  );
}

function problemsOf(error: ApiError, docType: DocTypeMeta): Problems | null {
  if (error.collection && error.line !== null) {
    const entry = {
      line: error.field ? undefined : error.message,
      fields: error.field ? { [error.field]: error.message } : {},
    };
    return { header: {}, lines: { [error.collection]: { [error.line]: entry } } };
  }
  if (
    error.field &&
    (error.field === "doc_date" || error.field === "memo" || docType.fields.some((f) => f.name === error.field))
  ) {
    return { header: { [error.field]: error.message }, lines: {} };
  }
  return null;
}

// ------------------------------------------------------------------ line grid

function ColumnHeader({ field, editable }: { field: FieldMeta; editable: boolean }) {
  const label = useDecimalHeader(field);
  return (
    <TableHead className={cn(isNumeric(field) && "text-end")} title={field.help ?? undefined}>
      {label}
      {editable && field.required && !field.readonly ? (
        <>
          <span aria-hidden="true" className="text-destructive">
            {t("field.required_mark")}
          </span>
          <span className="sr-only">{t("field.required")}</span>
        </>
      ) : null}
    </TableHead>
  );
}

/** One kind a line can be, and the fields that make it that kind. */
interface LineKind {
  label: string;
  fields: string[];
}

function LineGrid({
  collection,
  lines,
  editable,
  companyId,
  header,
  headerFields,
  problems,
  kinds,
  onChange,
}: {
  collection: CollectionMeta;
  /** What a line is, one of (stock, a charge, an account), as the document type declares. */
  kinds: LineKind[] | null;
  lines: Line[];
  editable: boolean;
  companyId: string | null;
  /** The document's header values, which references on a line can depend on. */
  header: FieldValues;
  headerFields: FieldMeta[];
  problems: Problems["lines"][string] | undefined;
  onChange: (lines: Line[], touched?: { line: number; field: string }) => void;
}) {
  const [focusLine, setFocusLine] = useState<string | null>(null);
  const gridRef = useRef<HTMLTableSectionElement>(null);

  // After a line is added, the cursor goes to its first field.
  useEffect(() => {
    if (!focusLine) return;
    const row = gridRef.current?.querySelector(`[data-line="${focusLine}"]`);
    row?.querySelector<HTMLElement>("input, select, button[role='combobox']")?.focus();
    setFocusLine(null);
  }, [focusLine]);

  function add(): void {
    const line = newLine(collection);
    onChange([...lines, line]);
    setFocusLine(line.key);
  }

  function remove(key: string): void {
    onChange(lines.filter((line) => line.key !== key));
  }

  function onCellKey(event: ReactKeyboardEvent<HTMLElement>, line: Line, isLast: boolean): void {
    // Enter in a field of the last line starts the next line.
    if (
      event.key === "Enter" &&
      isLast &&
      event.target instanceof HTMLInputElement &&
      event.target.type !== "checkbox"
    ) {
      event.preventDefault();
      add();
    } else if ((event.ctrlKey || event.metaKey) && event.key === "Delete") {
      event.preventDefault();
      remove(line.key);
    }
  }

  const fields = lineFields(collection);
  const contextFields = [...headerFields, ...collection.fields];
  // The fields of the kinds share one column, where the first of them would be.
  const kindFields = new Set((kinds ?? []).flatMap((kind) => kind.fields));
  const firstKindField = fields.find((field) => kindFields.has(field.name))?.name;
  const visible = fields.filter(
    (field) =>
      field.name === firstKindField || (!kindFields.has(field.name) && (editable || !isEmptyColumn(field, lines))),
  );
  // A line with nothing of any kind filled in yet is of the kind chosen for it, or the first.
  const [chosenKind, setChosenKind] = useState<Record<string, number>>({});
  const fieldByName = (name: string): FieldMeta | undefined => fields.find((field) => field.name === name);
  // A kind made of a line of the document the header names (an order line) cannot be had while
  // the header names none, as on an invoice without an order.
  const usable = (kind: LineKind): boolean =>
    kind.fields.every((name) => {
      const type = fieldByName(name)?.type;
      return type?.kind !== "line_link" || !isBlank(header[type.document_field]);
    });
  const firstUsable = Math.max(0, (kinds ?? []).findIndex(usable));
  const kindOf = (line: Line): number => {
    const filled = (kinds ?? []).findIndex((kind) => kind.fields.some((name) => !isBlank(line.values[name])));
    return filled >= 0 ? filled : (chosenKind[line.key] ?? firstUsable);
  };

  function changeKind(line: Line, next: number): void {
    setChosenKind((previous) => ({ ...previous, [line.key]: next }));
    // A line is one kind: the fields of the others are emptied.
    const cleared: FieldValues = { ...line.values };
    (kinds ?? []).forEach((kind, index) => {
      if (index !== next) for (const name of kind.fields) cleared[name] = null;
    });
    onChange(lines.map((other) => (other.key === line.key ? { ...other, values: cleared } : other)));
  }

  return (
    <div className="rounded-md border bg-card">
      <Table aria-label={collection.label}>
        <TableHeader>
          <TableRow>
            <TableHead className="w-8 text-end">{t("lines.number")}</TableHead>
            {visible.map((field) =>
              field.name === firstKindField && kinds ? (
                <TableHead key="kind">{kinds.map((kind) => kind.label).join(t("lines.kind_or"))}</TableHead>
              ) : (
                <ColumnHeader key={field.name} field={field} editable={editable} />
              ),
            )}
            {editable ? <TableHead className="w-8" /> : null}
          </TableRow>
        </TableHeader>
        <TableBody ref={gridRef}>
          {lines.map((line, index) => {
            const number = index + 1;
            const problem = problems?.[number];
            const context: FieldContext = { header, line: line.values, fields: contextFields };
            return [
              <TableRow
                key={line.key}
                data-line={line.key}
                className={cn(problem && "bg-destructive-soft/40")}
                aria-invalid={problem ? true : undefined}
              >
                <TableCell className="text-end text-xs tabular-nums text-muted-foreground">{number}</TableCell>
                {visible.map((field) => {
                  if (field.name === firstKindField && kinds) {
                    const active = kindOf(line);
                    const kind = kinds[active];
                    const kindErrors = (kind?.fields ?? [])
                      .map((name) => problem?.fields[name])
                      .filter((message): message is string => Boolean(message));
                    return (
                      <TableCell key="kind" className={cn(editable ? "px-1 py-1 align-top" : "whitespace-nowrap")}>
                        <div className="flex items-start gap-1">
                          {editable && kinds.length > 1 ? (
                            <Select
                              className="w-32 shrink-0"
                              aria-label={t("lines.kind", { line: number })}
                              value={kinds[active]?.label ?? ""}
                              onChange={(event) =>
                                changeKind(
                                  line,
                                  kinds.findIndex((option) => option.label === event.target.value),
                                )
                              }
                            >
                              {kinds.map((option) => (
                                <option key={option.label} value={option.label}>
                                  {option.label}
                                </option>
                              ))}
                            </Select>
                          ) : null}
                          {(kind?.fields ?? []).map((name) => {
                            const part = fieldByName(name);
                            if (!part) return null;
                            return editable && !part.readonly ? (
                              <div key={name} className="min-w-52">
                                <FieldInput
                                  field={part}
                                  value={line.values[name]}
                                  companyId={companyId}
                                  context={context}
                                  invalid={Boolean(problem?.fields[name])}
                                  aria-label={t("lines.cell", { field: part.label, line: number })}
                                  onKeyDown={(event) => onCellKey(event, line, index === lines.length - 1)}
                                  onChange={(value) =>
                                    onChange(
                                      lines.map((other) =>
                                        other.key === line.key
                                          ? { ...other, values: { ...other.values, [name]: value } }
                                          : other,
                                      ),
                                      { line: number, field: name },
                                    )
                                  }
                                />
                              </div>
                            ) : (
                              <span key={name} className="me-2">
                                <FieldValue field={part} value={line.values[name]} context={context} linkify />
                              </span>
                            );
                          })}
                        </div>
                        {kindErrors.map((message) => (
                          <p key={message} role="alert" className="px-1 pt-0.5 text-start text-xs text-destructive">
                            {message}
                          </p>
                        ))}
                      </TableCell>
                    );
                  }
                  const cellEditable = editable && !field.readonly;
                  const error = problem?.fields[field.name];
                  const errorId = `line-${collection.name}-${number}-${field.name}-error`;
                  return (
                    <TableCell
                      key={field.name}
                      className={cn(
                        cellEditable ? "min-w-28 px-1 py-1 align-top" : "whitespace-nowrap",
                        (field.type.kind === "link" ||
                          field.type.kind === "line_link" ||
                          field.type.kind === "lookup" ||
                          field.type.kind === "document_link") &&
                          "min-w-52",
                        isNumeric(field) && "text-end",
                      )}
                    >
                      {cellEditable ? (
                        <>
                          <FieldInput
                            field={field}
                            value={line.values[field.name]}
                            companyId={companyId}
                            context={context}
                            invalid={Boolean(error)}
                            aria-describedby={error ? errorId : undefined}
                            aria-label={t("lines.cell", { field: field.label, line: number })}
                            onKeyDown={(event) => onCellKey(event, line, index === lines.length - 1)}
                            onChange={(value) =>
                              onChange(
                                lines.map((other) =>
                                  other.key === line.key
                                    ? { ...other, values: { ...other.values, [field.name]: value } }
                                    : other,
                                ),
                                { line: number, field: field.name },
                              )
                            }
                          />
                          {error ? (
                            <p id={errorId} role="alert" className="px-1 pt-0.5 text-start text-xs text-destructive">
                              {error}
                            </p>
                          ) : null}
                        </>
                      ) : (
                        <>
                          <FieldValue field={field} value={line.values[field.name]} context={context} linkify />
                          {error ? (
                            <p role="alert" className="text-xs text-destructive">
                              {error}
                            </p>
                          ) : null}
                        </>
                      )}
                    </TableCell>
                  );
                })}
                {editable ? (
                  <TableCell className="px-1 py-1 align-top">
                    {/* Out of the Tab order so Tab runs field to field; Ctrl+Delete removes the line from the keyboard. */}
                    <Button
                      variant="ghost"
                      size="icon"
                      tabIndex={-1}
                      aria-label={t("lines.remove", { line: number })}
                      title={t("lines.remove_hint", { line: number })}
                      onClick={() => remove(line.key)}
                    >
                      <X aria-hidden="true" />
                    </Button>
                  </TableCell>
                ) : null}
              </TableRow>,
              problem?.line ? (
                <TableRow key={`${line.key}-problem`} className="bg-destructive-soft/40 hover:bg-destructive-soft/40">
                  <TableCell />
                  <TableCell colSpan={visible.length + (editable ? 1 : 0)} className="pt-0 text-xs text-destructive">
                    <span role="alert">{problem.line}</span>
                  </TableCell>
                </TableRow>
              ) : null,
            ];
          })}
        </TableBody>
      </Table>
      {lines.length === 0 ? (
        <p className="px-3 py-4 text-center text-sm text-muted-foreground">
          {t("lines.empty", { label: collection.label })}
        </p>
      ) : null}
      {editable ? (
        <div className="flex items-center gap-3 border-t px-2 py-1.5">
          <Button variant="ghost" size="sm" onClick={add}>
            <Plus aria-hidden="true" />
            {t("lines.add")}
          </Button>
          <span className="hidden text-xs text-muted-foreground md:inline">{t("lines.hint")}</span>
        </div>
      ) : null}
    </div>
  );
}

/** A read-only column with no value on any line is left out of a locked document. */
function isEmptyColumn(field: FieldMeta, lines: Line[]): boolean {
  return lines.length > 0 && lines.every((line) => isBlank(line.values[field.name]));
}

// ------------------------------------------------------------------ totals

/** The figures the document type keeps in its summary, such as its totals. */
function Totals({ docType, doc }: { docType: DocTypeMeta; doc: DocumentRecord }) {
  const { company } = useCompany();
  const context: FieldContext = { header: doc.summary, fields: docType.summary };
  const row = { values: doc.summary, fields: docType.summary };
  const currencyOf = (field: FieldMeta | undefined): string | null | undefined =>
    field ? plainSource(field.decimal?.currency ?? null, row, company?.base_currency ?? null) : undefined;
  const all = docType.summary.filter((field) => field.type.kind === "decimal");
  // A figure in the accounting currency (`total_gross_accounting`) beside its twin in the
  // document's currency says something only when the two currencies differ.
  const figures = all.filter((field) => {
    const twin = field.name.endsWith(ACCOUNTING)
      ? all.find((other) => `${other.name}${ACCOUNTING}` === field.name)
      : undefined;
    return !twin || currencyOf(twin) !== currencyOf(field);
  });
  // A document written before its type kept a summary has none; nothing is shown rather than a guess.
  if (figures.length === 0 || !figures.some((field) => field.name in doc.summary)) return null;
  return (
    <dl className="ms-auto grid w-full max-w-sm grid-cols-[1fr_auto] gap-x-6 gap-y-1 rounded-md border bg-card p-3 text-sm">
      {figures.map((field, index) => (
        <TotalRow
          key={field.name}
          field={field}
          value={doc.summary[field.name]}
          context={context}
          last={index === figures.length - 1}
        />
      ))}
    </dl>
  );
}

/** The ending of a summary figure in the accounting currency, beside its twin in the document's. */
const ACCOUNTING = "_accounting";

function TotalRow({
  field,
  value,
  context,
  last,
}: {
  field: FieldMeta;
  value: Json | undefined;
  context: FieldContext;
  last: boolean;
}) {
  const label = useDecimalHeader(field);
  return (
    <>
      <dt
        className={cn("text-muted-foreground", last && "border-t pt-1 font-medium text-foreground")}
        title={field.help ?? undefined}
      >
        {label}
      </dt>
      <dd className={cn("text-end", last && "border-t pt-1 font-semibold")}>
        {isBlank(value) ? (
          <span className="text-muted-foreground" title={field.help ?? undefined}>
            {t("document.not_known_yet")}
          </span>
        ) : (
          <FieldValue field={field} value={value} context={context} />
        )}
      </dd>
    </>
  );
}

// ------------------------------------------------------------------ the page

interface Conflict {
  latest: DocumentRecord;
  changes: string[];
}

function RequiredMark() {
  return (
    <>
      <span aria-hidden="true" className="text-destructive">
        {t("field.required_mark")}
      </span>
      <span className="sr-only">{t("field.required")}</span>
    </>
  );
}

function DocumentForm({ docType, id }: { docType: DocTypeMeta; id: string | null }) {
  const { meta, companies } = useApp();
  const { code, company } = useCompany();
  const navigate = useNavigate();
  const url = useUrlState();
  const isNew = id === null;
  const tab = url.get("tab") === "history" && !isNew ? "history" : "document";
  const headerMeta = (name: string): FieldMeta | undefined => meta.document_header.find((f) => f.name === name);
  const label = (name: string, fallback: string): string => headerMeta(name)?.label ?? fallback;

  const loaded = useAsync(id ? `${docType.name}|${id}` : null, (signal) =>
    api.run("document.get", { doc_type: docType.name, id: id ?? "" }, { signal }),
  );
  const [doc, setDoc] = useState<DocumentRecord | null>(null);
  // A new document starts with what the address names, such as the asset a disposal is of.
  const [form, setForm] = useState<FormState>(() => {
    const blank = blankForm(docType);
    if (!isNew) return blank;
    for (const field of docType.fields) {
      const named = url.get(field.name);
      if (!field.readonly && named !== "") blank.header[field.name] = named;
    }
    return blank;
  });
  const [baseline, setBaseline] = useState<string>(() => snapshot(docType, form));
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [problems, setProblems] = useState<Problems>(NO_PROBLEMS);
  const [conflict, setConflict] = useState<Conflict | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<ApiError | null>(null);

  function adopt(next: DocumentRecord): void {
    const nextForm = formFromDocument(docType, next);
    rememberDocument(next);
    setDoc(next);
    setForm(nextForm);
    setBaseline(snapshot(docType, nextForm));
    setConflict(null);
    setProblems(NO_PROBLEMS);
  }

  // After a create the page already holds the document (the create's answer), and the user may have
  // typed since: the same version read again must not replace what they typed.
  const held = useRef<DocumentRecord | null>(null);
  held.current = doc;
  const typed = useRef(false);
  // biome-ignore lint/correctness/useExhaustiveDependencies: runs when the server's copy arrives
  useEffect(() => {
    const next = loaded.data;
    if (!next) return;
    const current = held.current;
    // A reload after an action (a line matched) brings what changed, even at the same version.
    if (typed.current && current && current.id === next.id && current.version === next.version) return;
    adopt(next);
  }, [loaded.data]);

  // A document belongs to one company. If the URL names another, follow the document.
  const owner = doc ? companies.find((candidate) => candidate.id === doc.company_id) : undefined;
  useEffect(() => {
    if (doc && owner && owner.code !== code) navigate(paths.doc(owner.code, docType.name, doc.id), { replace: true });
  }, [doc, owner, code, docType.name, navigate]);

  const locked = doc?.locked ?? false;
  const canWrite = isNew ? canDoc(meta, docType.name, "create") : !locked && canDoc(meta, docType.name, "update");
  const current = snapshot(docType, form);
  const dirty = canWrite && current !== baseline;
  typed.current = dirty;
  const { blocker, allowNext } = useUnsavedGuard(dirty);
  const ready = isNew || doc !== null;
  const docCompany = owner ?? company;
  const header: FieldValues = { ...form.header, company_id: docCompany?.id ?? null, doc_date: form.docDate };

  const keyRef = useRef<{ payload: string; key: string }>({ payload: "", key: newKey() });
  function keyFor(payload: string): string {
    if (keyRef.current.payload !== payload) keyRef.current = { payload, key: newKey() };
    return keyRef.current.key;
  }

  function update(change: (previous: FormState) => FormState, clears?: (previous: Problems) => Problems): void {
    setForm(change);
    setSaved(false);
    if (clears) setProblems(clears);
  }

  const clearHeader =
    (name: string) =>
    (previous: Problems): Problems => {
      if (!(name in previous.header)) return previous;
      const { [name]: _, ...rest } = previous.header;
      return { ...previous, header: rest };
    };

  async function save(): Promise<void> {
    if (!canWrite || saving || !ready || (!dirty && !isNew)) return;
    setSaving(true);
    setFormError(null);
    setProblems(NO_PROBLEMS);
    setConflict(null);
    const body = bodyOf(docType, form);
    try {
      if (isNew) {
        if (!company) return;
        const payload = {
          doc_type: docType.name,
          company_id: company.id,
          doc_date: form.docDate,
          memo: form.memo || null,
          body,
        };
        const created = await api.run("document.create", payload, { idempotencyKey: keyFor(JSON.stringify(payload)) });
        allowNext();
        navigate(paths.doc(code, docType.name, created.id), { replace: true });
      } else if (doc) {
        const payload = {
          doc_type: docType.name,
          id: doc.id,
          version: doc.version,
          doc_date: form.docDate,
          memo: form.memo,
          body,
        };
        const updated = await api.run("document.update", payload, { idempotencyKey: keyFor(JSON.stringify(payload)) });
        adopt(updated);
        setSaved(true);
      }
    } catch (cause) {
      const error = cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected"));
      if (error.isStale && doc) {
        try {
          const latest = await api.run("document.get", { doc_type: docType.name, id: doc.id });
          if (latest.version !== doc.version) {
            const changes: string[] = [];
            if (latest.status !== doc.status)
              changes.push(
                t("conflict.doc_status", {
                  status: docType.states.find((state) => state.name === latest.status)?.label ?? latest.status,
                }),
              );
            if (latest.doc_date !== doc.doc_date)
              changes.push(t("conflict.doc_date", { date: formatCalendarDate(latest.doc_date) }));
            if ((latest.memo ?? "") !== (doc.memo ?? ""))
              changes.push(t("conflict.doc_memo", { memo: latest.memo ?? "" }));
            if (JSON.stringify(latest.body) !== JSON.stringify(doc.body)) changes.push(t("conflict.doc_body"));
            setConflict({ latest, changes });
            setSaving(false);
            return;
          }
        } catch {
          // Fall through to the server's own words.
        }
      }
      const placed = problemsOf(error, docType);
      if (placed) setProblems(placed);
      setFormError(error.message);
    } finally {
      setSaving(false);
    }
  }

  const saveRef = useRef(save);
  saveRef.current = save;
  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        // While a dialog is open the page underneath is not what the user is working on.
        if (document.querySelector("[role='dialog']")) return;
        void saveRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  async function remove(): Promise<void> {
    if (!doc) return;
    setDeleting(true);
    setDeleteError(null);
    try {
      await api.run("document.delete", { doc_type: docType.name, id: doc.id });
      allowNext();
      forgetDocuments();
      navigate(paths.docList(code, docType.name), { replace: true });
    } catch (cause) {
      setDeleteError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
      setDeleting(false);
    }
  }

  const status = doc?.status ?? docType.initial_state;
  const actions = useMemo(() => (doc ? recordActions(meta, docType.name, doc.status) : []), [meta, docType.name, doc]);
  const followOn = useFollowOn((created, createdId) => {
    forgetDocuments();
    navigate(paths.doc(code, created, createdId));
  });
  // The same actions in the command palette, each run as its button runs it.
  useOfferPageActions(
    doc && !dirty
      ? {
          subject: `${docType.label} ${doc.number ?? t("document.unnumbered")}`,
          actions: actions.map((action) => ({
            id: action.command.name,
            label: action.label,
            destructive: action.destructive,
            run: () => {
              if (action.creates) void followOn.run(action, doc.id);
              else url.set({ action: action.action });
            },
          })),
        }
      : null,
  );
  const actionName = url.get("action");
  const openAction =
    doc && !dirty ? actions.find((action) => action.action === actionName && !action.creates) : undefined;

  const number = doc?.number ?? null;
  const subject = `${docType.label} ${number ?? t("document.unnumbered")}`;
  const crumbs = [{ label: docType.plural, to: paths.docList(code, docType.name) }];
  const history = useMemo(() => (doc ? `${doc.version}` : ""), [doc]);
  const contextFields = [...meta.document_header, ...docType.fields];

  if (!isNew && loaded.error && !doc) {
    return (
      <>
        <PageHeader crumbs={crumbs} title={docType.label} />
        {loaded.error.status === 404 ? (
          <NotFoundPage />
        ) : (
          <Alert
            title={t("record.load_failed")}
            actions={
              <Button variant="outline" size="sm" onClick={loaded.reload}>
                {t("action.retry")}
              </Button>
            }
          >
            {loaded.error.message}
          </Alert>
        )}
      </>
    );
  }

  const fieldError = (name: string): string | undefined => problems.header[name];
  const lineProblemCount = Object.values(problems.lines).reduce((sum, lines) => sum + Object.keys(lines).length, 0);

  return (
    <>
      <PageHeader
        sticky
        crumbs={crumbs}
        title={
          !ready ? (
            <Skeleton className="h-6 w-56" />
          ) : isNew ? (
            t("record.new", { label: docType.label })
          ) : (
            <>
              {docType.label}{" "}
              <span className={cn("tabular-nums", !number && "font-normal text-muted-foreground")}>
                {number ?? t("document.unnumbered")}
              </span>
            </>
          )
        }
        badges={ready ? <StatusBadge status={status} docType={docType} /> : <Skeleton className="h-5 w-14" />}
        actions={
          <>
            {saved && !dirty ? (
              <output className="text-sm text-muted-foreground">{t("record.saved")}</output>
            ) : dirty ? (
              <output className="text-sm text-muted-foreground">{t("record.unsaved")}</output>
            ) : null}
            {doc ? (
              <ActionButtons
                actions={actions}
                running={followOn.running}
                disabledReason={dirty ? t("record.save_first") : null}
                onChoose={(action) => {
                  if (action.creates) void followOn.run(action, doc.id);
                  else url.set({ action: action.action });
                }}
              />
            ) : null}
            {doc ? <EinvoiceExportButton docType={docType.name} id={doc.id} locked={locked} /> : null}
            {doc && canRun(meta, "document.render") ? (
              <Button asChild variant="outline">
                <Link to={paths.print(code, docType.name, doc.id)}>
                  <Printer aria-hidden="true" />
                  {t("print.print")}
                </Link>
              </Button>
            ) : null}
            {doc && !locked && doc.postings.length === 0 && canDoc(meta, docType.name, "delete") ? (
              <Button variant="destructive-outline" onClick={() => setConfirmDelete(true)}>
                <Trash2 aria-hidden="true" />
                {t("action.delete")}
              </Button>
            ) : null}
            {canWrite ? (
              <Button
                onClick={() => void save()}
                busy={saving}
                disabled={!ready || (!dirty && !isNew)}
                title={t("record.save_hint")}
              >
                {isNew ? t("record.create") : t("action.save")}
              </Button>
            ) : null}
          </>
        }
      />

      {!isNew ? (
        <Tabs
          current={tab}
          tabs={[
            { id: "document", label: t("document.tab"), to: "?" },
            { id: "history", label: t("history.title"), to: "?tab=history" },
          ]}
        />
      ) : null}

      <div hidden={tab !== "document"} className="space-y-5">
        {conflict ? (
          <Alert
            tone="warning"
            title={t("conflict.title")}
            actions={
              <>
                <Button variant="outline" size="sm" onClick={() => adopt(conflict.latest)}>
                  {t("conflict.take_theirs")}
                </Button>
                {conflict.latest.locked ? null : (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => {
                      // Keep what was typed; the next save knowingly replaces the newer version.
                      setDoc(conflict.latest);
                      setConflict(null);
                    }}
                  >
                    {t("conflict.keep_mine")}
                  </Button>
                )}
              </>
            }
          >
            <p>{t("conflict.body")}</p>
            {conflict.changes.length > 0 ? (
              <ul className="mt-1 list-disc space-y-0.5 ps-5">
                {conflict.changes.map((change) => (
                  <li key={change}>{change}</li>
                ))}
              </ul>
            ) : null}
          </Alert>
        ) : null}
        {formError ? (
          <Alert title={t("record.save_failed")}>
            {formError}
            {lineProblemCount > 0 ? <p className="mt-1">{t("document.see_marked_line")}</p> : null}
          </Alert>
        ) : null}
        {followOn.pending ? <ApprovalNotice approval={followOn.pending} preview={false} /> : null}
        {followOn.error ? (
          <Alert
            title={t("follow_on.failed")}
            actions={
              <Button variant="outline" size="sm" onClick={followOn.clear}>
                {t("action.close")}
              </Button>
            }
          >
            {followOn.error.message}
          </Alert>
        ) : null}
        {doc && locked ? (
          <Alert tone="info">
            {t("document.locked", { status: docType.states.find((s) => s.name === status)?.label ?? status })}
          </Alert>
        ) : null}

        <div className="grid gap-x-4 gap-y-3 rounded-md border bg-card p-4 sm:grid-cols-2 xl:grid-cols-4">
          <div className="space-y-1">
            <Label htmlFor="doc-date">
              {label("doc_date", t("document.date"))}
              {canWrite ? <RequiredMark /> : null}
            </Label>
            {!ready ? (
              <Skeleton className="h-8 w-full" />
            ) : canWrite ? (
              <DateInput
                id="doc-date"
                value={form.docDate}
                aria-required="true"
                aria-invalid={fieldError("doc_date") ? true : undefined}
                onChange={(docDate) => update((previous) => ({ ...previous, docDate }), clearHeader("doc_date"))}
              />
            ) : (
              <div className="flex min-h-8 items-center text-sm">{formatCalendarDate(form.docDate)}</div>
            )}
            {fieldError("doc_date") ? (
              <p role="alert" className="text-xs text-destructive">
                {fieldError("doc_date")}
              </p>
            ) : null}
          </div>
          {docType.fields.map((field) => {
            // A new document started without its source (an invoice without an order) does not offer it.
            if (isNew && field.name === url.get("without")) return null;
            const editable = canWrite && !field.readonly;
            const fieldId = `header-${field.name}`;
            const error = fieldError(field.name);
            const hintId = `${fieldId}-hint`;
            return (
              <div key={field.name} className="space-y-1">
                <Label htmlFor={editable ? fieldId : undefined}>
                  {field.label}
                  {field.required && editable ? <RequiredMark /> : null}
                </Label>
                {!ready ? (
                  <Skeleton className="h-8 w-full" />
                ) : editable ? (
                  <FieldInput
                    id={fieldId}
                    field={field}
                    value={form.header[field.name]}
                    companyId={docCompany?.id}
                    context={{ header, fields: contextFields }}
                    invalid={Boolean(error)}
                    aria-describedby={error ? `${fieldId}-error` : field.help ? hintId : undefined}
                    onChange={(value) =>
                      update(
                        (previous) => ({ ...previous, header: { ...previous.header, [field.name]: value } }),
                        clearHeader(field.name),
                      )
                    }
                  />
                ) : (
                  <div className="flex min-h-8 items-center text-sm">
                    <FieldValue
                      field={field}
                      value={form.header[field.name]}
                      context={{ header, fields: contextFields }}
                      linkify
                    />
                  </div>
                )}
                {error ? (
                  <p id={`${fieldId}-error`} role="alert" className="text-xs text-destructive">
                    {error}
                  </p>
                ) : editable && field.help ? (
                  <p id={hintId} className="text-xs text-muted-foreground">
                    {field.help}
                  </p>
                ) : null}
              </div>
            );
          })}
          <div className="space-y-1 sm:col-span-2">
            <Label htmlFor={canWrite ? "doc-memo" : undefined}>{label("memo", t("document.memo"))}</Label>
            {!ready ? (
              <Skeleton className="h-8 w-full" />
            ) : canWrite ? (
              <Input
                id="doc-memo"
                value={form.memo}
                aria-invalid={fieldError("memo") ? true : undefined}
                onChange={(event) =>
                  update((previous) => ({ ...previous, memo: event.target.value }), clearHeader("memo"))
                }
              />
            ) : (
              <div className="flex min-h-8 items-center text-sm">
                {form.memo || <span className="text-muted-foreground">{t("value.none")}</span>}
              </div>
            )}
            {fieldError("memo") ? (
              <p role="alert" className="text-xs text-destructive">
                {fieldError("memo")}
              </p>
            ) : null}
          </div>
          {doc ? (
            <p className="text-xs text-muted-foreground sm:col-span-2 xl:col-span-4">
              {doc.created_by_name
                ? t("document.created_by", {
                    label: label("created_by_name", t("document.created")),
                    name: doc.created_by_name,
                    at: formatTimestamp(doc.created_at),
                  })
                : t("document.created_at", {
                    label: label("created_at", t("document.created")),
                    at: formatTimestamp(doc.created_at),
                  })}
              {t("value.separator")}
              {t("document.changed_at", {
                label: label("updated_at", t("document.updated")),
                at: formatTimestamp(doc.updated_at),
              })}
            </p>
          ) : null}
        </div>

        {doc && docType.name === BANK_STATEMENT && doc.status === docType.initial_state && !dirty ? (
          // A draft bank statement is matched line by line rather than edited as a grid.
          <BankMatching doc={doc} docType={docType} onChanged={loaded.reload} />
        ) : null}
        {docType.collections
          .filter(() => !(doc && docType.name === BANK_STATEMENT && doc.status === docType.initial_state && !dirty))
          .map((collection) => (
            <Section key={collection.name} title={collection.label}>
              {ready && serverLines(collection) && (form.collections[collection.name] ?? []).length === 0 ? (
                <p className="rounded-md border bg-card p-4 text-sm text-muted-foreground">
                  {doc ? t("document.no_server_lines") : t("document.server_lines")}
                </p>
              ) : ready ? (
                <LineGrid
                  collection={collection}
                  lines={form.collections[collection.name] ?? []}
                  editable={canWrite && !serverLines(collection)}
                  kinds={
                    docType.alternatives.find((alternative) => alternative.collection === collection.name)?.one_of ??
                    null
                  }
                  companyId={docCompany?.id ?? null}
                  header={header}
                  headerFields={contextFields}
                  problems={problems.lines[collection.name]}
                  onChange={(lines, touched) =>
                    update(
                      (previous) => ({
                        ...previous,
                        collections: { ...previous.collections, [collection.name]: lines },
                      }),
                      touched
                        ? (previous) => {
                            const line = previous.lines[collection.name]?.[touched.line];
                            if (!line || !(touched.field in line.fields)) return previous;
                            const { [touched.field]: _, ...fields } = line.fields;
                            return {
                              ...previous,
                              lines: {
                                ...previous.lines,
                                [collection.name]: {
                                  ...previous.lines[collection.name],
                                  [touched.line]: { ...line, fields },
                                },
                              },
                            };
                          }
                        : undefined,
                    )
                  }
                />
              ) : (
                <Skeleton className="h-24 w-full" />
              )}
            </Section>
          ))}

        {doc && !dirty ? <Totals docType={docType} doc={doc} /> : null}

        {doc && doc.links.length > 0 ? (
          <Section title={t("document.links")}>
            <ul className="divide-y rounded-md border bg-card text-sm">
              {doc.links.map((link) => {
                const linked = findDocType(meta, link.doc_type);
                return (
                  <li
                    key={`${link.relation}:${link.id}`}
                    className="flex flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2"
                  >
                    <span className="w-28 text-xs text-muted-foreground">
                      {link.relation === "source" ? t("document.link_source") : t("document.link_follow_up")}
                    </span>
                    <Link
                      to={paths.doc(code, link.doc_type, link.id)}
                      className="font-medium underline underline-offset-2 hover:no-underline"
                    >
                      {linked?.label ?? link.doc_type} {link.number ?? t("document.unnumbered")}
                    </Link>
                    <StatusBadge status={link.status} docType={linked} />
                    <span className="text-muted-foreground">{formatCalendarDate(link.doc_date)}</span>
                  </li>
                );
              })}
            </ul>
          </Section>
        ) : null}

        {doc && doc.postings.length > 0 ? (
          <Section title={t("postings.title")}>
            <Postings postings={doc.postings} currency={docCompany?.base_currency ?? ""} />
          </Section>
        ) : null}
      </div>

      {tab === "history" && doc ? <History resource={docType.name} id={doc.id} version={history} /> : null}

      <UnsavedDialog blocker={blocker} />
      <ConfirmDialog
        open={confirmDelete}
        title={t("delete.title", { label: subject })}
        description={t("delete.description")}
        confirmLabel={t("action.delete")}
        busy={deleting}
        error={deleteError}
        onConfirm={() => void remove()}
        onClose={() => {
          setConfirmDelete(false);
          setDeleteError(null);
        }}
      />
      {openAction && doc ? (
        <ActionDialog
          action={openAction}
          input={{ id: doc.id }}
          subject={subject}
          current={doc}
          fields={meta.document_header}
          docType={docType}
          onClose={() => url.set({ action: null }, { replace: true })}
          onDone={() => {
            url.set({ action: null }, { replace: true });
            forgetDocuments();
            loaded.reload();
          }}
        />
      ) : null}
    </>
  );
}
