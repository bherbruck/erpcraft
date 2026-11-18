// Party data (`party.get`, `party.set`): what the company, a customer or a vendor is known by
// outside the books. Not a generic entity, so it has a panel on the owner's record page and a
// page of its own to edit it, both driven by the two commands.

import { Pencil, Plus, Trash2 } from "lucide-react";
import { type FormEvent, type ReactNode, useEffect, useMemo, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { ApiError, api } from "@/api/client";
import type { CommandInputs, CommandOutputs } from "@/api/generated";
import { useApp, useCompany } from "@/app/context";
import { Alert } from "@/components/alert";
import { Badge } from "@/components/badge";
import { Button } from "@/components/button";
import { Lookup } from "@/components/lookup";
import { PageHeader, Section } from "@/components/page";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { Skeleton } from "@/components/ui/skeleton";
import { UnsavedDialog, useUnsavedGuard } from "@/components/unsaved-guard";
import { type MessageKey, t } from "@/i18n/t";
import { useLinkInfo } from "@/lib/link-labels";
import { canEntity, canRun, findEntity } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useAsync } from "@/lib/use-async";

type PartyType = CommandInputs["party.get"]["party_type"];
type Party = CommandOutputs["party.get"];
type SetInput = CommandInputs["party.set"];
type Address = NonNullable<SetInput["addresses"]>[number];
type TaxId = NonNullable<SetInput["tax_ids"]>[number];
type Electronic = NonNullable<SetInput["electronic_addresses"]>[number];
type Contact = NonNullable<SetInput["contacts"]>[number];
type Profile = NonNullable<SetInput["profile"]>;

const PARTY_TYPES: readonly string[] = ["company", "customer", "vendor"];
const TAX_SCHEMES = ["eu_vat", "tax_number", "us_ein", "ca_bn", "other"] as const;
const ADDRESS_KINDS = ["billing", "shipping"] as const;

export function isParty(entity: string): entity is PartyType {
  return PARTY_TYPES.includes(entity);
}

function schemeLabel(scheme: string): string {
  return (TAX_SCHEMES as readonly string[]).includes(scheme) ? t(`party.scheme.${scheme}` as MessageKey) : scheme;
}

function kindLabel(kind: string): string {
  return (ADDRESS_KINDS as readonly string[]).includes(kind) ? t(`party.kind.${kind}` as MessageKey) : kind;
}

function addressLines(address: Party["addresses"][number]): string[] {
  return [
    address.attention,
    address.street,
    address.additional_street,
    [address.postcode, address.city].filter(Boolean).join(" "),
    [address.region, address.country_code].filter(Boolean).join(", "),
  ].filter((line): line is string => Boolean(line));
}

function Entry({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid gap-1 sm:grid-cols-3 sm:gap-4">
      <dt className="text-sm text-muted-foreground">{label}</dt>
      <dd className="text-sm sm:col-span-2">{children}</dd>
    </div>
  );
}

function AccountName({ id }: { id: string }) {
  const info = useLinkInfo("account", "id", id);
  return <>{info === undefined ? t("value.loading") : (info?.label ?? id)}</>;
}

/** The party data of a company, customer or vendor, shown under its record. */
export function PartyPanel({ partyType, id }: { partyType: PartyType; id: string }) {
  const { meta } = useApp();
  const { code } = useCompany();
  const party = useAsync(`party|${partyType}|${id}`, (signal) =>
    api.run("party.get", { party_type: partyType, party_id: id }, { signal }),
  );
  if (!canRun(meta, "party.get")) return null;
  const editable = canRun(meta, "party.set") && canEntity(meta, partyType, "update");
  const data = party.data;
  const profile = data?.profile;
  const empty =
    data !== undefined &&
    !profile &&
    data.addresses.length === 0 &&
    data.tax_ids.length === 0 &&
    data.electronic_addresses.length === 0 &&
    data.contacts.length === 0;

  return (
    <Section
      title={t("party.title")}
      aside={
        editable ? (
          <Button asChild variant="outline" size="sm">
            <Link to={`${paths.entityRecord(code, partyType, id)}/party`}>
              <Pencil aria-hidden="true" />
              {t("party.edit")}
            </Link>
          </Button>
        ) : null
      }
    >
      {party.error ? <Alert>{party.error.message}</Alert> : null}
      {data === undefined && !party.error ? <Skeleton className="h-24 w-full" /> : null}
      {empty ? <p className="rounded-lg border bg-card p-4 text-sm text-muted-foreground">{t("party.none")}</p> : null}
      {data && !empty ? (
        <div className="grid gap-4 lg:grid-cols-2">
          {profile ? (
            <Card className="gap-3 py-4">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">{t("party.profile")}</CardTitle>
              </CardHeader>
              <CardContent className="px-4">
                <dl className="space-y-2">
                  {profile.legal_name ? <Entry label={t("party.legal_name")}>{profile.legal_name}</Entry> : null}
                  {profile.trading_name ? <Entry label={t("party.trading_name")}>{profile.trading_name}</Entry> : null}
                  {profile.legal_registration ? (
                    <Entry label={t("party.legal_registration")}>{profile.legal_registration}</Entry>
                  ) : null}
                  {profile.legal_info ? (
                    <Entry label={t("party.legal_info")}>
                      <span className="whitespace-pre-line">{profile.legal_info}</span>
                    </Entry>
                  ) : null}
                  {profile.invoice_reference ? (
                    <Entry label={t("party.invoice_reference")}>{profile.invoice_reference}</Entry>
                  ) : null}
                  {profile.payment_account_id ? (
                    <Entry label={t("party.payment_account")}>
                      <AccountName id={profile.payment_account_id} />
                    </Entry>
                  ) : null}
                </dl>
              </CardContent>
            </Card>
          ) : null}
          {data.addresses.length > 0 ? (
            <Card className="gap-3 py-4">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">{t("party.addresses")}</CardTitle>
              </CardHeader>
              <CardContent className="grid gap-4 px-4 sm:grid-cols-2">
                {data.addresses.map((address, index) => (
                  <address key={address.id ?? index} className="space-y-1 text-sm not-italic">
                    <p className="flex flex-wrap items-center gap-2 font-medium">
                      {kindLabel(address.kind)}
                      {address.is_default ? <Badge>{t("party.default")}</Badge> : null}
                    </p>
                    {addressLines(address).map((line) => (
                      <p key={line}>{line}</p>
                    ))}
                  </address>
                ))}
              </CardContent>
            </Card>
          ) : null}
          {data.tax_ids.length > 0 || data.electronic_addresses.length > 0 ? (
            <Card className="gap-3 py-4">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">{t("party.identifiers")}</CardTitle>
              </CardHeader>
              <CardContent className="px-4">
                <dl className="space-y-2">
                  {data.tax_ids.map((taxId) => (
                    <Entry key={taxId.scheme} label={schemeLabel(taxId.scheme)}>
                      {taxId.value}
                      {taxId.country_code ? (
                        <span className="ms-2 text-muted-foreground">{taxId.country_code}</span>
                      ) : null}
                    </Entry>
                  ))}
                  {data.electronic_addresses.map((address) => (
                    <Entry
                      key={`${address.scheme}:${address.value}`}
                      label={t("party.electronic_address", { scheme: address.scheme })}
                    >
                      {address.value}
                      {address.is_default ? <Badge className="ms-2">{t("party.default")}</Badge> : null}
                    </Entry>
                  ))}
                </dl>
              </CardContent>
            </Card>
          ) : null}
          {data.contacts.length > 0 ? (
            <Card className="gap-3 py-4">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">{t("party.contacts")}</CardTitle>
              </CardHeader>
              <CardContent className="space-y-3 px-4">
                {data.contacts.map((contact, index) => (
                  <div
                    // biome-ignore lint/suspicious/noArrayIndexKey: contacts have no id of their own
                    key={index}
                    className="space-y-0.5 text-sm"
                  >
                    <p className="flex flex-wrap items-center gap-2 font-medium">
                      {contact.name ?? t("value.none")}
                      {contact.role ? <span className="font-normal text-muted-foreground">{contact.role}</span> : null}
                      {contact.is_default ? <Badge>{t("party.default")}</Badge> : null}
                    </p>
                    {contact.email ? <p>{contact.email}</p> : null}
                    {contact.phone ? <p>{contact.phone}</p> : null}
                  </div>
                ))}
              </CardContent>
            </Card>
          ) : null}
        </div>
      ) : null}
    </Section>
  );
}

// ------------------------------------------------------------------ editing

interface Draft {
  profile: Profile;
  addresses: Address[];
  tax_ids: TaxId[];
  electronic_addresses: Electronic[];
  contacts: Contact[];
}

function draftOf(party: Party): Draft {
  const profile = party.profile;
  return {
    profile: {
      legal_name: profile?.legal_name ?? null,
      trading_name: profile?.trading_name ?? null,
      legal_registration: profile?.legal_registration ?? null,
      legal_info: profile?.legal_info ?? null,
      invoice_reference: profile?.invoice_reference ?? null,
      payment_account_id: profile?.payment_account_id ?? null,
    },
    addresses: party.addresses.map((address) => ({ ...address })),
    tax_ids: party.tax_ids.map((taxId) => ({ ...taxId })),
    electronic_addresses: party.electronic_addresses.map((address) => ({ ...address })),
    contacts: party.contacts.map((contact) => ({ ...contact })),
  };
}

/** Empty text is no value. */
function text(value: string): string | null {
  return value.trim() === "" ? null : value;
}

function TextField({
  id,
  label,
  value,
  onChange,
  required = false,
  error,
}: {
  id: string;
  label: string;
  value: string | null | undefined;
  onChange: (value: string | null) => void;
  required?: boolean;
  error?: string | null;
}) {
  return (
    <div className="space-y-2">
      <Label htmlFor={id}>
        {label}
        {required ? (
          <span className="text-destructive" aria-hidden="true">
            {t("field.required_mark")}
          </span>
        ) : null}
      </Label>
      <Input
        id={id}
        value={value ?? ""}
        required={required}
        aria-invalid={error ? true : undefined}
        aria-describedby={error ? `${id}-error` : undefined}
        onChange={(event) => onChange(text(event.target.value))}
      />
      {error ? (
        <p id={`${id}-error`} role="alert" className="text-sm text-destructive">
          {error}
        </p>
      ) : null}
    </div>
  );
}

function DefaultBox({ id, checked, onChange }: { id: string; checked: boolean; onChange: (next: boolean) => void }) {
  return (
    <Label htmlFor={id} className="font-normal">
      <input id={id} type="checkbox" checked={checked} onChange={(event) => onChange(event.target.checked)} />
      {t("party.default")}
    </Label>
  );
}

/** A part of the form that is a list of rows, each in a card with a Remove button. */
function Rows<Row>({
  title,
  rows,
  onChange,
  blank,
  addLabel,
  render,
}: {
  title: string;
  rows: Row[];
  onChange: (rows: Row[]) => void;
  blank: Row;
  addLabel: string;
  render: (row: Row, update: (patch: Partial<Row>) => void, index: number) => ReactNode;
}) {
  return (
    <Section
      title={title}
      aside={
        <Button variant="outline" size="sm" onClick={() => onChange([...rows, blank])}>
          <Plus aria-hidden="true" />
          {addLabel}
        </Button>
      }
    >
      {rows.length === 0 ? (
        <p className="rounded-lg border bg-card p-4 text-sm text-muted-foreground">{t("party.part_none")}</p>
      ) : null}
      {rows.map((row, index) => (
        <Card
          // biome-ignore lint/suspicious/noArrayIndexKey: rows are edited in place and have no stable key of their own
          key={index}
          className="gap-0 py-4"
        >
          <CardContent className="flex items-start gap-3 px-4">
            <div className="grid min-w-0 flex-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
              {render(row, (patch) => onChange(rows.map((r, i) => (i === index ? { ...r, ...patch } : r))), index)}
            </div>
            <Button
              variant="ghost"
              size="icon"
              aria-label={t("party.remove", { part: title, number: index + 1 })}
              title={t("party.remove", { part: title, number: index + 1 })}
              onClick={() => onChange(rows.filter((_, i) => i !== index))}
            >
              <Trash2 aria-hidden="true" />
            </Button>
          </CardContent>
        </Card>
      ))}
    </Section>
  );
}

/** `/c/:company/e/:entity/:id/party`: edits a company's, customer's or vendor's party data. */
export function PartyEditPage() {
  const { entity: entityName = "", id = "" } = useParams();
  const { meta } = useApp();
  const { code, company } = useCompany();
  const entity = findEntity(meta, entityName);
  const partyType = isParty(entityName) ? entityName : null;
  const party = useAsync(partyType ? `party|${partyType}|${id}` : null, (signal) =>
    api.run("party.get", { party_type: partyType ?? "company", party_id: id }, { signal }),
  );
  const owner = useAsync(entity ? `party-owner|${entityName}|${id}` : null, (signal) =>
    api.run("entity.get", { entity: entityName, id }, { signal }),
  );
  const loaded = useMemo(() => (party.data ? draftOf(party.data) : null), [party.data]);
  const [draft, setDraft] = useState<Draft | null>(null);
  useEffect(() => setDraft(loaded), [loaded]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [key] = useState(() => crypto.randomUUID());
  const navigate = useNavigate();
  const dirty = draft !== null && loaded !== null && JSON.stringify(draft) !== JSON.stringify(loaded);
  const { blocker, allowNext } = useUnsavedGuard(dirty);
  const back = paths.entityRecord(code, entityName, id);

  async function save(event?: FormEvent): Promise<void> {
    event?.preventDefault();
    if (!draft || !partyType || busy) return;
    setBusy(true);
    setError(null);
    try {
      await api.run(
        "party.set",
        {
          party_type: partyType,
          party_id: id,
          profile: draft.profile,
          addresses: draft.addresses,
          tax_ids: draft.tax_ids,
          electronic_addresses: draft.electronic_addresses,
          contacts: draft.contacts,
        },
        { idempotencyKey: key },
      );
      allowNext();
      navigate(back);
    } catch (cause) {
      // What was typed stays; the server's words are shown where they belong.
      setError(cause instanceof ApiError ? cause : new ApiError(0, "unexpected", t("error.unexpected")));
      setBusy(false);
    }
  }

  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        void save();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  if (!entity || !partyType) {
    return <Alert>{t("party.not_a_party", { entity: entityName })}</Alert>;
  }
  const ownerName = owner.data && typeof owner.data.name === "string" ? owner.data.name : null;
  // The server names the part, the row (from 1) and the field it refuses.
  const errorAt = (collection: string | null, line: number | null, field: string): string | null =>
    error && error.field === field && error.collection === collection && error.line === line ? error.message : null;
  const placed =
    error !== null &&
    (draft?.addresses.some((_, i) => ["country_code", "kind"].some((f) => errorAt("addresses", i + 1, f))) ||
      ["legal_name", "invoice_reference"].some((f) => errorAt(null, null, f)));

  const set = <K extends keyof Draft>(part: K, value: Draft[K]): void =>
    setDraft((current) => (current ? { ...current, [part]: value } : current));
  const setProfile = (patch: Partial<Profile>): void =>
    setDraft((current) => (current ? { ...current, profile: { ...current.profile, ...patch } } : current));

  return (
    <>
      <PageHeader
        sticky
        crumbs={[
          { label: entity.plural, to: paths.entityList(code, entityName) },
          { label: ownerName ?? entity.label, to: back },
        ]}
        title={t("party.title")}
        badges={dirty ? <Badge tone="warning">{t("record.unsaved")}</Badge> : null}
        actions={
          <>
            <Button asChild variant="outline">
              <Link to={back}>{t("action.cancel")}</Link>
            </Button>
            <Button type="submit" form="party-form" busy={busy} disabled={!dirty}>
              {t("action.save")}
            </Button>
          </>
        }
      />
      {party.error ? <Alert>{party.error.message}</Alert> : null}
      {draft === null && !party.error ? <Skeleton className="h-64 w-full" /> : null}
      {draft ? (
        <form id="party-form" noValidate onSubmit={(event) => void save(event)} className="max-w-5xl space-y-8">
          {error && !placed ? <Alert title={t("party.not_saved")}>{error.message}</Alert> : null}
          <Section title={t("party.profile")}>
            <Card className="py-4">
              <CardContent className="grid gap-4 px-4 sm:grid-cols-2">
                <TextField
                  id="party-legal_name"
                  label={t("party.legal_name")}
                  value={draft.profile.legal_name}
                  onChange={(legal_name) => setProfile({ legal_name })}
                  error={errorAt(null, null, "legal_name")}
                />
                <TextField
                  id="party-trading_name"
                  label={t("party.trading_name")}
                  value={draft.profile.trading_name}
                  onChange={(trading_name) => setProfile({ trading_name })}
                />
                <TextField
                  id="party-legal_registration"
                  label={t("party.legal_registration")}
                  value={draft.profile.legal_registration}
                  onChange={(legal_registration) => setProfile({ legal_registration })}
                />
                <TextField
                  id="party-legal_info"
                  label={t("party.legal_info")}
                  value={draft.profile.legal_info}
                  onChange={(legal_info) => setProfile({ legal_info })}
                />
                {partyType === "customer" ? (
                  <TextField
                    id="party-invoice_reference"
                    label={t("party.invoice_reference")}
                    value={draft.profile.invoice_reference}
                    onChange={(invoice_reference) => setProfile({ invoice_reference })}
                    error={errorAt(null, null, "invoice_reference")}
                  />
                ) : null}
                {partyType === "company" ? (
                  <div className="space-y-2">
                    <Label htmlFor="party-payment_account_id">{t("party.payment_account")}</Label>
                    <Lookup
                      id="party-payment_account_id"
                      entity="account"
                      keyField="id"
                      companyId={company?.id ?? null}
                      filters={{ is_bank: "true" }}
                      allowCreate={false}
                      value={draft.profile.payment_account_id ?? null}
                      onChange={(payment_account_id) => setProfile({ payment_account_id })}
                    />
                  </div>
                ) : null}
              </CardContent>
            </Card>
          </Section>

          <Rows
            title={t("party.addresses")}
            addLabel={t("party.add_address")}
            rows={draft.addresses}
            onChange={(rows) => set("addresses", rows)}
            blank={{ kind: "billing", is_default: draft.addresses.length === 0, country_code: "" }}
            render={(address, update, index) => {
              const at = (field: string) => `party-address-${index}-${field}`;
              return (
                <>
                  <div className="space-y-2">
                    <Label htmlFor={at("kind")}>{t("party.kind")}</Label>
                    <NativeSelect
                      id={at("kind")}
                      className="w-full"
                      value={address.kind ?? "billing"}
                      onChange={(event) => update({ kind: event.target.value })}
                    >
                      {ADDRESS_KINDS.map((kind) => (
                        <option key={kind} value={kind}>
                          {kindLabel(kind)}
                        </option>
                      ))}
                    </NativeSelect>
                  </div>
                  <TextField
                    id={at("attention")}
                    label={t("party.attention")}
                    value={address.attention}
                    onChange={(attention) => update({ attention })}
                  />
                  <div className="flex items-end pb-2">
                    <DefaultBox
                      id={at("default")}
                      checked={address.is_default ?? false}
                      onChange={(is_default) => update({ is_default })}
                    />
                  </div>
                  <TextField
                    id={at("street")}
                    label={t("party.street")}
                    value={address.street}
                    onChange={(street) => update({ street })}
                  />
                  <TextField
                    id={at("additional_street")}
                    label={t("party.additional_street")}
                    value={address.additional_street}
                    onChange={(additional_street) => update({ additional_street })}
                  />
                  <TextField
                    id={at("postcode")}
                    label={t("party.postcode")}
                    value={address.postcode}
                    onChange={(postcode) => update({ postcode })}
                  />
                  <TextField
                    id={at("city")}
                    label={t("party.city")}
                    value={address.city}
                    onChange={(city) => update({ city })}
                  />
                  <TextField
                    id={at("region")}
                    label={t("party.region")}
                    value={address.region}
                    onChange={(region) => update({ region })}
                  />
                  <TextField
                    id={at("country_code")}
                    label={t("party.country")}
                    required
                    value={address.country_code}
                    onChange={(country_code) => update({ country_code: country_code ?? "" })}
                    error={errorAt("addresses", index + 1, "country_code")}
                  />
                </>
              );
            }}
          />

          <Rows
            title={t("party.tax_ids")}
            addLabel={t("party.add_tax_id")}
            rows={draft.tax_ids}
            onChange={(rows) => set("tax_ids", rows)}
            blank={{ scheme: "eu_vat", value: "", country_code: null }}
            render={(taxId, update, index) => (
              <>
                <div className="space-y-2">
                  <Label htmlFor={`party-tax-${index}-scheme`}>{t("party.scheme")}</Label>
                  <NativeSelect
                    id={`party-tax-${index}-scheme`}
                    className="w-full"
                    value={taxId.scheme}
                    onChange={(event) => update({ scheme: event.target.value })}
                  >
                    {TAX_SCHEMES.map((scheme) => (
                      <option key={scheme} value={scheme}>
                        {schemeLabel(scheme)}
                      </option>
                    ))}
                  </NativeSelect>
                </div>
                <TextField
                  id={`party-tax-${index}-value`}
                  label={t("party.value")}
                  required
                  value={taxId.value}
                  onChange={(value) => update({ value: value ?? "" })}
                />
                <TextField
                  id={`party-tax-${index}-country`}
                  label={t("party.issued_by")}
                  value={taxId.country_code}
                  onChange={(country_code) => update({ country_code })}
                />
              </>
            )}
          />

          <Rows
            title={t("party.electronic_addresses")}
            addLabel={t("party.add_electronic_address")}
            rows={draft.electronic_addresses}
            onChange={(rows) => set("electronic_addresses", rows)}
            blank={{ scheme: "EM", value: "", is_default: draft.electronic_addresses.length === 0 }}
            render={(address, update, index) => (
              <>
                <TextField
                  id={`party-eas-${index}-scheme`}
                  label={t("party.eas_scheme")}
                  value={address.scheme}
                  onChange={(scheme) => update({ scheme: scheme ?? "EM" })}
                />
                <TextField
                  id={`party-eas-${index}-value`}
                  label={t("party.value")}
                  required
                  value={address.value}
                  onChange={(value) => update({ value: value ?? "" })}
                />
                <div className="flex items-end pb-2">
                  <DefaultBox
                    id={`party-eas-${index}-default`}
                    checked={address.is_default ?? false}
                    onChange={(is_default) => update({ is_default })}
                  />
                </div>
              </>
            )}
          />

          <Rows
            title={t("party.contacts")}
            addLabel={t("party.add_contact")}
            rows={draft.contacts}
            onChange={(rows) => set("contacts", rows)}
            blank={{ is_default: draft.contacts.length === 0 }}
            render={(contact, update, index) => (
              <>
                <TextField
                  id={`party-contact-${index}-name`}
                  label={t("party.contact_name")}
                  value={contact.name}
                  onChange={(name) => update({ name })}
                />
                <TextField
                  id={`party-contact-${index}-role`}
                  label={t("party.contact_role")}
                  value={contact.role}
                  onChange={(role) => update({ role })}
                />
                <div className="flex items-end pb-2">
                  <DefaultBox
                    id={`party-contact-${index}-default`}
                    checked={contact.is_default ?? false}
                    onChange={(is_default) => update({ is_default })}
                  />
                </div>
                <TextField
                  id={`party-contact-${index}-email`}
                  label={t("party.email")}
                  value={contact.email}
                  onChange={(email) => update({ email })}
                />
                <TextField
                  id={`party-contact-${index}-phone`}
                  label={t("party.phone")}
                  value={contact.phone}
                  onChange={(phone) => update({ phone })}
                />
              </>
            )}
          />
          {/* Enter in a field saves, like the button in the header. */}
          <button type="submit" hidden tabIndex={-1} aria-hidden="true" />
        </form>
      ) : null}
      <UnsavedDialog blocker={blocker} />
    </>
  );
}
