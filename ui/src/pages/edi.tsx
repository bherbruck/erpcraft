// EDI: receiving an interchange (`edi.receive`), and reviewing a received order
// before it becomes a sales order (`edi.proposal`, `edi.draft`, `edi.dismiss`).
// What the server found is shown as it says it; the user chooses only what the
// server could not: an item, a price, a warehouse, a ship-to.

import { Ban, FilePlus2, Upload } from "lucide-react";
import { type FormEvent, type ReactNode, useMemo, useRef, useState } from "react";
import { Link, useNavigate } from "react-router";
import { toast } from "sonner";
import { ApiError, api } from "@/api/client";
import type { CommandOutputs, EdiDraftInput, EntityName } from "@/api/generated";
import { useApp, useCompany } from "@/app/context";
import { Alert } from "@/components/alert";
import { Badge, type BadgeTone } from "@/components/badge";
import { Button } from "@/components/button";
import { Dialog } from "@/components/dialog";
import { FormField } from "@/components/form-field";
import { Lookup } from "@/components/lookup";
import { MoneyInput } from "@/components/money-input";
import { PageHeader, Section } from "@/components/page";
import { Card, CardContent } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { formatDecimal } from "@/lib/decimal";
import { canRun, enumLabel, findCommand, findEntity } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

export const EDI_RECEIVE = "edi.receive";
export const EDI_MESSAGE: EntityName = "edi_message";

type Received = CommandOutputs["edi.receive"];
type Proposal = CommandOutputs["edi.proposal"];

const STATE_TONES: Record<string, BadgeTone> = {
  proposed: "info",
  drafted: "success",
  dismissed: "neutral",
  refused: "destructive",
  failed: "destructive",
  read: "success",
};

/** A message's state in the words the registry gives it. */
function StateBadge({ state }: { state: string }) {
  const { meta } = useApp();
  const field = findEntity(meta, EDI_MESSAGE)?.fields.find((candidate) => candidate.name === "state");
  return <Badge tone={STATE_TONES[state] ?? "neutral"}>{field ? enumLabel(field, state) : state}</Badge>;
}

/** `/c/:company/run/edi.receive`: an interchange file, read in the browser and handed to the server. */
export function EdiReceivePage() {
  const { meta } = useApp();
  const command = findCommand(meta, EDI_RECEIVE);
  if (!command || !canRun(meta, command.name)) return <NotFoundPage />;
  return <EdiReceive label={command.label} description={command.description} />;
}

function EdiReceive({ label, description }: { label: string; description: string }) {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const messages = findEntity(meta, EDI_MESSAGE);
  const fileRef = useRef<HTMLInputElement>(null);
  const [file, setFile] = useState<File | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [received, setReceived] = useState<Received | null>(null);

  async function receive(event: FormEvent): Promise<void> {
    event.preventDefault();
    if (!file) return;
    setBusy(true);
    setError(null);
    setReceived(null);
    try {
      const content = await file.text();
      setReceived(await api.run(EDI_RECEIVE, { content, company_id: company?.id ?? null }));
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setBusy(false);
  }

  return (
    <>
      <PageHeader
        crumbs={messages ? [{ label: messages.plural, to: paths.entityList(code, EDI_MESSAGE) }] : undefined}
        title={label}
      />
      <p className="mb-4 max-w-form text-sm text-muted-foreground">{description}</p>
      <form noValidate onSubmit={(event) => void receive(event)} className="max-w-form space-y-4">
        {error ? <Alert title={t("edi.not_received")}>{error.message}</Alert> : null}
        <Card className="py-4">
          <CardContent className="px-4">
            <FormField id="edi-file" label={t("edi.file")} hint={t("edi.file_hint")}>
              <Input
                ref={fileRef}
                id="edi-file"
                type="file"
                accept=".x12,.edi,.txt,text/plain"
                onChange={(event) => setFile(event.target.files?.[0] ?? null)}
              />
            </FormField>
          </CardContent>
        </Card>
        <Button type="submit" busy={busy} disabled={!file}>
          <Upload aria-hidden="true" />
          {label}
        </Button>
      </form>
      {received ? (
        <Section title={t("edi.received")}>
          <div className="max-w-form space-y-3">
            {received.repeat ? <Alert tone="info">{t("edi.repeat")}</Alert> : null}
            {received.interchange.error ? (
              <Alert title={t("edi.interchange_refused")}>{received.interchange.error}</Alert>
            ) : null}
            <ul className="divide-y rounded-lg border bg-card text-sm">
              {received.messages.map((message) => (
                <li key={message.id} className="flex flex-wrap items-center gap-3 px-4 py-3">
                  <Link
                    to={paths.entityRecord(code, EDI_MESSAGE, message.id)}
                    className="font-medium underline underline-offset-2 hover:no-underline"
                  >
                    {t("edi.message_line", { message: message.message, key: message.business_key ?? message.id })}
                  </Link>
                  <StateBadge state={message.state} />
                </li>
              ))}
            </ul>
            {received.acknowledgements.length > 0 ? (
              <p className="text-sm text-muted-foreground">
                {t("edi.acknowledged", { count: received.acknowledgements.length })}
              </p>
            ) : null}
          </div>
        </Section>
      ) : null}
    </>
  );
}

/** Under a received message: the sales order it would become, what stands in the way, and the choice. */
export function EdiProposalPanel({ messageId }: { messageId: string }) {
  const { meta } = useApp();
  const proposal = useAsync(`edi-proposal|${messageId}`, (signal) =>
    api.run("edi.proposal", { message_id: messageId }, { signal }),
  );
  if (!canRun(meta, "edi.proposal")) return null;
  return (
    <Section title={t("edi.proposal")}>
      {proposal.error ? <Alert>{proposal.error.message}</Alert> : null}
      {proposal.data === undefined && !proposal.error ? <Skeleton className="h-48 w-full" /> : null}
      {proposal.data ? (
        <ProposalReview proposal={proposal.data} messageId={messageId} onChanged={proposal.reload} />
      ) : null}
      <Explained messageId={messageId} />
    </Section>
  );
}

function ProposalReview({
  proposal,
  messageId,
  onChanged,
}: {
  proposal: Proposal;
  messageId: string;
  onChanged: () => void;
}) {
  const { meta } = useApp();
  const { code } = useCompany();
  const navigate = useNavigate();
  const [items, setItems] = useState<Record<string, string>>({});
  const [prices, setPrices] = useState<Record<string, string>>({});
  const [warehouse, setWarehouse] = useState<string | null>(proposal.warehouse_id);
  const [remember, setRemember] = useState(true);
  const [acceptNotes, setAcceptNotes] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [dismissing, setDismissing] = useState(false);
  const [key] = useState(() => crypto.randomUUID());
  const open = proposal.state === "proposed";
  const blocking = proposal.findings.filter((finding) => finding.severity === "blocking");
  const notes = proposal.findings.filter((finding) => finding.severity === "note");
  const chosen = Object.keys(items).length > 0;
  const order = proposal.order;
  const orderNumber =
    order !== null && typeof order === "object" && !Array.isArray(order) && typeof order.number === "string"
      ? order.number
      : null;
  const sent = proposal.ship_to.sent;
  const shipTo =
    sent !== null && typeof sent === "object" && !Array.isArray(sent)
      ? [sent.name, ...(Array.isArray(sent.address_lines) ? sent.address_lines : []), sent.city, sent.postal_code]
          .filter((part): part is string => typeof part === "string" && part !== "")
          .join(", ")
      : "";

  async function draft(): Promise<void> {
    setBusy(true);
    setError(null);
    const input: EdiDraftInput = {
      message_id: messageId,
      items: chosen ? items : null,
      prices: Object.keys(prices).length > 0 ? prices : null,
      warehouse_id: warehouse !== proposal.warehouse_id ? warehouse : null,
      remember: chosen && remember,
      accept_notes: acceptNotes,
    };
    try {
      const made = await api.run("edi.draft", input, { idempotencyKey: key });
      toast.success(t("edi.drafted", { number: orderNumber ?? "" }));
      navigate(paths.doc(code, made.doc_type, made.id));
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
      setBusy(false);
    }
  }

  return (
    <div className="space-y-4">
      <Card className="py-4">
        <CardContent className="grid gap-4 px-4 sm:grid-cols-2 xl:grid-cols-4">
          <Fact label={t("edi.partner")}>{proposal.partner.name}</Fact>
          <Fact label={t("edi.customer")}>
            {proposal.customer ? (
              <Link
                to={paths.entityRecord(code, "customer", proposal.customer.id)}
                className="underline underline-offset-2 hover:no-underline"
              >
                {`${proposal.customer.code} · ${proposal.customer.name}`}
              </Link>
            ) : (
              t("value.none")
            )}
          </Fact>
          <Fact label={t("edi.order")}>
            {[orderNumber, proposal.doc_date ? formatCalendarDate(proposal.doc_date) : null]
              .filter(Boolean)
              .join(" · ")}
          </Fact>
          <Fact label={t("edi.state")}>
            <StateBadge state={proposal.state} />
          </Fact>
          <Fact label={t("edi.ship_to")}>
            {shipTo || t("value.none")}
            <span className="block text-muted-foreground">{t("edi.ship_to_how", { how: proposal.ship_to.how })}</span>
          </Fact>
          <div className="space-y-1">
            <Label htmlFor="edi-warehouse" className="text-muted-foreground">
              {t("edi.warehouse")}
            </Label>
            {open ? (
              <Lookup
                id="edi-warehouse"
                entity="warehouse"
                keyField="id"
                companyId={proposal.company_id}
                allowCreate={false}
                value={warehouse}
                onChange={(next) => setWarehouse(next)}
              />
            ) : null}
          </div>
          <Fact label={t("edi.total_net")}>
            <span className="tabular-nums">
              {proposal.total_net === null ? t("value.none") : `${proposal.total_net} ${proposal.currency ?? ""}`}
            </span>
          </Fact>
        </CardContent>
      </Card>

      {proposal.findings.length > 0 ? (
        <ul className="space-y-2">
          {proposal.findings.map((finding, index) => (
            <li
              // biome-ignore lint/suspicious/noArrayIndexKey: findings have no id; the list does not change while shown
              key={index}
              className="flex flex-wrap items-start gap-2 rounded-lg border bg-card px-4 py-3 text-sm"
            >
              <Badge tone={finding.severity === "blocking" ? "destructive" : "warning"}>
                {finding.severity === "blocking" ? t("edi.blocking") : t("edi.note")}
              </Badge>
              {finding.line ? <span className="font-medium">{t("edi.line", { line: finding.line })}</span> : null}
              <span className="min-w-0 flex-1">{finding.message}</span>
            </li>
          ))}
        </ul>
      ) : (
        <Alert tone="info">{t("edi.no_findings")}</Alert>
      )}

      <div className="overflow-hidden rounded-lg border bg-card">
        <Table aria-label={t("edi.lines")}>
          <TableHeader>
            <TableRow>
              <TableHead className="px-3">{t("edi.line_id")}</TableHead>
              <TableHead className="px-3">{t("edi.sent")}</TableHead>
              <TableHead className="px-3 text-end">{t("edi.their_qty")}</TableHead>
              <TableHead className="px-3 text-end">{t("edi.their_price")}</TableHead>
              <TableHead className="min-w-64 px-3">{t("edi.item")}</TableHead>
              <TableHead className="px-3 text-end">{t("edi.our_qty")}</TableHead>
              <TableHead className="px-3 text-end">{t("edi.unit_price")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {proposal.lines.map((line) => {
              const item = items[line.line_id] ?? line.item_id;
              const ids = Array.isArray(line.product_ids)
                ? line.product_ids
                    .map((id) =>
                      id !== null && typeof id === "object" && !Array.isArray(id) ? `${id.qualifier} ${id.value}` : "",
                    )
                    .filter(Boolean)
                    .join(" · ")
                : "";
              return (
                <TableRow key={line.line_id}>
                  <TableCell className="px-3 py-2.5 align-top tabular-nums">{line.line_id}</TableCell>
                  <TableCell className="px-3 py-2.5 align-top whitespace-normal">
                    <span className="block">{line.description ?? t("value.none")}</span>
                    <span className="block font-mono text-xs text-muted-foreground">{ids}</span>
                  </TableCell>
                  <TableCell className="px-3 py-2.5 text-end align-top tabular-nums">
                    {`${line.partner_qty ?? ""} ${line.partner_uom}`}
                  </TableCell>
                  <TableCell className="px-3 py-2.5 text-end align-top tabular-nums">
                    {line.partner_price ?? t("value.none")}
                  </TableCell>
                  <TableCell className="px-3 py-2 align-top">
                    {open ? (
                      <Lookup
                        aria-label={t("edi.item_for", { line: line.line_id })}
                        entity="item"
                        keyField="id"
                        allowCreate={false}
                        value={item}
                        onChange={(next) =>
                          setItems((previous) => {
                            const { [line.line_id]: _, ...rest } = previous;
                            return next && next !== line.item_id ? { ...rest, [line.line_id]: next } : rest;
                          })
                        }
                      />
                    ) : (
                      <span>{line.item_code ? `${line.item_code} · ${line.item_name ?? ""}` : t("value.none")}</span>
                    )}
                    {line.how_matched && !items[line.line_id] ? (
                      <span className="mt-1 block text-sm text-muted-foreground">
                        {t("edi.matched_by", { how: line.how_matched })}
                      </span>
                    ) : null}
                  </TableCell>
                  <TableCell className="px-3 py-2.5 text-end align-top tabular-nums">
                    {line.qty === null ? t("value.none") : `${formatDecimal(line.qty)} ${line.uom ?? ""}`}
                  </TableCell>
                  <TableCell className="px-3 py-2 text-end align-top">
                    {open && line.unit_price === null ? (
                      <MoneyInput
                        aria-label={t("edi.price_for", { line: line.line_id })}
                        className="w-28"
                        value={prices[line.line_id] ?? ""}
                        onChange={(next) => setPrices((previous) => ({ ...previous, [line.line_id]: next }))}
                      />
                    ) : (
                      <span className="tabular-nums">
                        {line.unit_price === null ? t("value.none") : formatDecimal(line.unit_price)}
                      </span>
                    )}
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </div>

      {open && canRun(meta, "edi.draft") ? (
        <div className="space-y-3">
          {error ? <Alert title={t("edi.not_drafted")}>{error.message}</Alert> : null}
          {chosen ? (
            <div className="flex items-center gap-2">
              <Checkbox id="edi-remember" checked={remember} onCheckedChange={(next) => setRemember(next === true)} />
              <Label htmlFor="edi-remember" className="font-normal">
                {t("edi.remember")}
              </Label>
            </div>
          ) : null}
          {notes.length > 0 ? (
            <div className="flex items-center gap-2">
              <Checkbox
                id="edi-accept-notes"
                checked={acceptNotes}
                onCheckedChange={(next) => setAcceptNotes(next === true)}
              />
              <Label htmlFor="edi-accept-notes" className="font-normal">
                {t("edi.accept_notes")}
              </Label>
            </div>
          ) : null}
          <div className="flex flex-wrap gap-2">
            <Button busy={busy} onClick={() => void draft()} disabled={notes.length > 0 && !acceptNotes}>
              <FilePlus2 aria-hidden="true" />
              {findCommand(meta, "edi.draft")?.label}
            </Button>
            {canRun(meta, "edi.dismiss") ? (
              <Button variant="destructive" onClick={() => setDismissing(true)} disabled={busy}>
                <Ban aria-hidden="true" />
                {findCommand(meta, "edi.dismiss")?.label}
              </Button>
            ) : null}
          </div>
          {notes.length > 0 && !acceptNotes ? (
            <p className="text-sm text-muted-foreground">{t("edi.notes_hint")}</p>
          ) : blocking.length > 0 && !chosen ? (
            <p className="text-sm text-muted-foreground">{t("edi.blocking_hint")}</p>
          ) : null}
        </div>
      ) : null}
      {dismissing ? (
        <DismissDialog
          messageId={messageId}
          onClose={() => setDismissing(false)}
          onDone={() => {
            setDismissing(false);
            onChanged();
          }}
        />
      ) : null}
    </div>
  );
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="space-y-1 text-sm">
      <p className="text-muted-foreground">{label}</p>
      <div>{children}</div>
    </div>
  );
}

function DismissDialog({ messageId, onClose, onDone }: { messageId: string; onClose: () => void; onDone: () => void }) {
  const { meta } = useApp();
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const label = findCommand(meta, "edi.dismiss")?.label ?? "";
  async function dismiss(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await api.run("edi.dismiss", { message_id: messageId, reason: reason.trim() });
      toast.success(t("edi.dismissed"));
      onDone();
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
      setBusy(false);
    }
  }
  return (
    <Dialog
      open
      onClose={onClose}
      dirty={reason !== "" || busy}
      title={label}
      description={t("edi.dismiss_hint")}
      footer={
        <>
          <Button variant="outline" autoFocus onClick={onClose} disabled={busy}>
            {t("action.cancel")}
          </Button>
          <Button variant="destructive" busy={busy} disabled={reason.trim() === ""} onClick={() => void dismiss()}>
            {label}
          </Button>
        </>
      }
    >
      <FormField
        id="edi-dismiss-reason"
        label={t("edi.reason")}
        required
        error={error?.field === "reason" ? error.message : null}
      >
        <Textarea id="edi-dismiss-reason" value={reason} onChange={(event) => setReason(event.target.value)} />
      </FormField>
      {error && error.field !== "reason" ? <Alert className="mt-3">{error.message}</Alert> : null}
    </Dialog>
  );
}

/** The file, segment by segment, each with a sentence that says what it means. */
function Explained({ messageId }: { messageId: string }) {
  const { meta } = useApp();
  const [open, setOpen] = useState(false);
  const explained = useAsync(open ? `edi-explain|${messageId}` : null, (signal) =>
    api.run("edi.explain", { message_id: messageId }, { signal }),
  );
  const segments = useMemo(() => explained.data?.segments ?? [], [explained.data]);
  if (!canRun(meta, "edi.explain")) return null;
  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <CollapsibleTrigger asChild>
        <Button variant="outline" size="sm">
          {open ? t("edi.hide_file") : t("edi.show_file")}
        </Button>
      </CollapsibleTrigger>
      <CollapsibleContent className="mt-3">
        {explained.error ? <Alert>{explained.error.message}</Alert> : null}
        {explained.data === undefined && !explained.error ? <Skeleton className="h-32 w-full" /> : null}
        {segments.length > 0 ? (
          <div className="overflow-hidden rounded-lg border bg-card">
            <Table aria-label={t("edi.file_segments")}>
              <TableBody>
                {segments.map((segment) => (
                  <TableRow key={segment.position}>
                    <TableCell className="w-12 px-3 py-2 align-top text-end tabular-nums text-muted-foreground">
                      {segment.position}
                    </TableCell>
                    <TableCell className="px-3 py-2 align-top font-mono text-sm whitespace-normal break-all">
                      {segment.text}
                    </TableCell>
                    <TableCell className="px-3 py-2 align-top whitespace-normal">{segment.says}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        ) : null}
      </CollapsibleContent>
    </Collapsible>
  );
}
