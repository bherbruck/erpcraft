//! Party data: what the company, a customer or a vendor is known by outside
//! the books. Its legal particulars (`party_profile`), postal addresses
//! (`party_address`, billing and shipping, a default of each), tax
//! identifiers with their scheme (`party_tax_id`: EU VAT, tax number, US
//! EIN, Canadian BN), electronic addresses (`party_electronic_address`,
//! with an EAS scheme such as a Peppol identifier) and contacts
//! (`party_contact`).
//!
//! One schema for the three kinds of party, keyed by `party_type` and
//! `party_id` as account rules are keyed by scope. The company, customer
//! and vendor records stay with the modules that own them, which need not
//! know this module.
//!
//! **Rights follow the owner.** The tables are not generic entities: they
//! are read with `party.get` and written with `party.set`, which first read
//! the owning record with the caller's rights (its company filter and row
//! rules included) and then ask the owner's own right on that record:
//! `customer:read` / `customer:update`, and the same for `vendor` and
//! `company`. There is no permission of the party module's own to get
//! around them. Printing and e-invoices read through [`load`],
//! [`load_readable`] and the command `document.parties`.

use std::sync::Arc;

use erpcraft_kernel::authz::Target;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, DynCommand, Error, JsonSchema, Migration, Module, PgConnection,
    Result, async_trait, command, document, entity, new_id, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub struct PartyModule;

impl Module for PartyModule {
    fn name(&self) -> &'static str {
        "party"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl", "sales", "purchasing"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![Migration {
            version: 1,
            name: "party",
            sql: include_str!("../migrations/0001_party.sql"),
        }]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![command(Get), command(Set), command(Parties)]
    }
}

/// The kind of party: the entity that owns the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Company,
    Customer,
    Vendor,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Company => "company",
            Kind::Customer => "customer",
            Kind::Vendor => "vendor",
        }
    }

    fn parse(text: &str) -> Result<Kind> {
        match text {
            "company" => Ok(Kind::Company),
            "customer" => Ok(Kind::Customer),
            "vendor" => Ok(Kind::Vendor),
            _ => Err(Error::field(
                "party_type",
                "must be company, customer or vendor",
            )),
        }
    }

    /// Which grants decide a change: the company's own for a company, the
    /// grant for all companies for a customer or vendor, which all
    /// companies share (as the entity layer decides for them).
    fn write_target(self, id: Uuid) -> Target {
        match self {
            Kind::Company => Target::Company(id),
            Kind::Customer | Kind::Vendor => Target::Global,
        }
    }
}

// ------------------------------------------------------------------- read

/// A postal address.
#[derive(Debug, Clone, Default, Serialize, JsonSchema, sqlx::FromRow)]
pub struct Address {
    pub attention: Option<String>,
    pub street: Option<String>,
    pub additional_street: Option<String>,
    pub city: Option<String>,
    pub postcode: Option<String>,
    pub region: Option<String>,
    pub country_code: String,
}

/// A party as documents show it: its particulars, its default billing
/// address, its tax identifiers by scheme, its default electronic address
/// and its default contact. A party with no rows has only its name.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct PartyData {
    pub kind: String,
    pub id: Uuid,
    /// The record's own name: the company's, customer's or vendor's.
    pub name: Option<String>,
    pub legal_name: Option<String>,
    pub trading_name: Option<String>,
    pub legal_registration: Option<String>,
    pub legal_info: Option<String>,
    pub invoice_reference: Option<String>,
    pub payment_account_id: Option<Uuid>,
    pub address: Option<Address>,
    /// `eu_vat`, `tax_number`, `us_ein`, `ca_bn` or `other`, and the value.
    pub tax_ids: Vec<(String, String)>,
    /// The scheme and the address.
    pub electronic_address: Option<(String, String)>,
    pub contact_name: Option<String>,
    pub contact_phone: Option<String>,
    pub contact_email: Option<String>,
}

impl PartyData {
    pub fn tax_id(&self, scheme: &str) -> Option<String> {
        self.tax_ids
            .iter()
            .find(|(s, _)| s == scheme)
            .map(|(_, v)| v.clone())
    }
}

/// The party's data, read whole: for a module's own use, such as writing
/// an e-invoice of a document the caller may read.
pub async fn load(conn: &mut PgConnection, kind: &str, id: Uuid) -> Result<PartyData> {
    let kind = Kind::parse(kind)?;
    let name: Option<String> =
        sqlx::query_scalar(&format!("SELECT name FROM {} WHERE id = $1", kind.as_str()))
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    let mut data = PartyData {
        kind: kind.as_str().into(),
        id,
        name,
        ..PartyData::default()
    };
    let kind = kind.as_str();
    let profile: Option<Profile> = sqlx::query_as(
        "SELECT legal_name, trading_name, legal_registration, legal_info, invoice_reference,
                payment_account_id
         FROM party_profile WHERE party_type = $1 AND party_id = $2",
    )
    .bind(kind)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(p) = profile {
        data.legal_name = p.legal_name;
        data.trading_name = p.trading_name;
        data.legal_registration = p.legal_registration;
        data.legal_info = p.legal_info;
        data.invoice_reference = p.invoice_reference;
        data.payment_account_id = p.payment_account_id;
    }
    // The default billing address; else the first billing address.
    data.address = sqlx::query_as(
        "SELECT attention, street, additional_street, city, postcode, region, country_code
         FROM party_address
         WHERE party_type = $1 AND party_id = $2 AND kind = 'billing'
         ORDER BY is_default DESC, created_at LIMIT 1",
    )
    .bind(kind)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    data.tax_ids = sqlx::query_as(
        "SELECT scheme, value FROM party_tax_id WHERE party_type = $1 AND party_id = $2
         ORDER BY scheme",
    )
    .bind(kind)
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    data.electronic_address = sqlx::query_as(
        "SELECT scheme, value FROM party_electronic_address
         WHERE party_type = $1 AND party_id = $2
         ORDER BY is_default DESC, created_at LIMIT 1",
    )
    .bind(kind)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    let contact: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT name, phone, email FROM party_contact WHERE party_type = $1 AND party_id = $2
         ORDER BY is_default DESC, created_at LIMIT 1",
    )
    .bind(kind)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((name, phone, email)) = contact {
        data.contact_name = name;
        data.contact_phone = phone;
        data.contact_email = email;
    }
    Ok(data)
}

/// The owner's record as the caller may read it; nothing if they may not.
pub async fn owner(ctx: &mut Ctx<'_>, kind: &str, id: Uuid) -> Result<Option<Value>> {
    Kind::parse(kind)?;
    match entity::get(ctx, kind, id).await {
        Ok(record) => Ok(Some(record)),
        Err(Error::NotFound(_) | Error::Forbidden(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The party's data if the caller may read its owner's record, as rights
/// follow the owner; nothing otherwise.
pub async fn load_readable(ctx: &mut Ctx<'_>, kind: &str, id: Uuid) -> Result<Option<PartyData>> {
    if owner(ctx, kind, id).await?.is_none() {
        return Ok(None);
    }
    Ok(Some(load(&mut *ctx.conn, kind, id).await?))
}

/// The vendors whose tax identifier of `scheme` is `value`, written with or
/// without spaces and punctuation, with the row that says so.
pub async fn vendors_by_tax_id(
    conn: &mut PgConnection,
    scheme: &str,
    value: &str,
) -> Result<Vec<(Uuid, Uuid)>> {
    let plain: String = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_uppercase();
    Ok(sqlx::query_as(
        "SELECT id, party_id FROM party_tax_id
         WHERE party_type = 'vendor' AND scheme = $1
           AND upper(regexp_replace(value, '[^A-Za-z0-9]', '', 'g')) = $2
         ORDER BY created_at LIMIT 10",
    )
    .bind(scheme)
    .bind(plain)
    .fetch_all(&mut *conn)
    .await?)
}

// ------------------------------------------------------ party.get and set

/// The legal particulars.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// The registered name, when it is not the record's name.
    pub legal_name: Option<String>,
    pub trading_name: Option<String>,
    /// The commercial register entry: "HRB 12345, Amtsgericht München".
    pub legal_registration: Option<String>,
    /// Legal form, share capital, managing directors.
    pub legal_info: Option<String>,
    /// For a customer: the reference it asks its invoices to quote; for a
    /// German public buyer its Leitweg-ID.
    pub invoice_reference: Option<String>,
    /// For the company: the bank account customers pay into.
    #[schemars(extend("x-field-type" = { "kind": "link", "entity": "account", "key": "id" }))]
    pub payment_account_id: Option<Uuid>,
}

/// A postal address of the party.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct AddressRecord {
    /// `billing` (where invoices go) or `shipping` (where goods go).
    #[serde(default = "billing")]
    pub kind: String,
    /// The party's address of this kind unless a document names another.
    /// At most one of each kind.
    #[serde(default)]
    pub is_default: bool,
    /// Who or which site at the address.
    pub attention: Option<String>,
    pub street: Option<String>,
    pub additional_street: Option<String>,
    pub city: Option<String>,
    pub postcode: Option<String>,
    /// State, province or region.
    pub region: Option<String>,
    /// ISO 3166-1 alpha-2, such as DE or US.
    pub country_code: String,
}

fn billing() -> String {
    "billing".into()
}

/// A tax identifier and its scheme.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct TaxIdRecord {
    /// `eu_vat`, `tax_number` (a national tax number, such as the German
    /// Steuernummer), `us_ein`, `ca_bn` or `other`. One of each.
    pub scheme: String,
    /// As the authority writes it, such as DE123456789.
    pub value: String,
    /// The country that issued it.
    pub country_code: Option<String>,
}

/// Where documents reach the party electronically.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct ElectronicAddressRecord {
    /// From the EAS code list: EM for email (the default), 0204 for a
    /// Leitweg-ID, 9930 for a German VAT number, 0088 for a GLN.
    #[serde(default = "email")]
    pub scheme: String,
    pub value: String,
    /// At most one.
    #[serde(default)]
    pub is_default: bool,
}

fn email() -> String {
    "EM".into()
}

/// Someone to talk to at the party.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct ContactRecord {
    pub name: Option<String>,
    /// What they do: accounts payable, sales.
    pub role: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    /// At most one.
    #[serde(default)]
    pub is_default: bool,
}

/// All a party has on record.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct PartyRecords {
    pub party_type: String,
    pub party_id: Uuid,
    pub profile: Option<Profile>,
    pub addresses: Vec<AddressRecord>,
    pub tax_ids: Vec<TaxIdRecord>,
    pub electronic_addresses: Vec<ElectronicAddressRecord>,
    pub contacts: Vec<ContactRecord>,
}

async fn records(conn: &mut PgConnection, kind: Kind, id: Uuid) -> Result<PartyRecords> {
    let k = kind.as_str();
    Ok(PartyRecords {
        party_type: k.into(),
        party_id: id,
        profile: sqlx::query_as(
            "SELECT legal_name, trading_name, legal_registration, legal_info, invoice_reference,
                    payment_account_id
             FROM party_profile WHERE party_type = $1 AND party_id = $2",
        )
        .bind(k)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?,
        addresses: sqlx::query_as(
            "SELECT kind, is_default, attention, street, additional_street, city, postcode,
                    region, country_code
             FROM party_address WHERE party_type = $1 AND party_id = $2
             ORDER BY kind, is_default DESC, created_at",
        )
        .bind(k)
        .bind(id)
        .fetch_all(&mut *conn)
        .await?,
        tax_ids: sqlx::query_as(
            "SELECT scheme, value, country_code FROM party_tax_id
             WHERE party_type = $1 AND party_id = $2 ORDER BY scheme",
        )
        .bind(k)
        .bind(id)
        .fetch_all(&mut *conn)
        .await?,
        electronic_addresses: sqlx::query_as(
            "SELECT scheme, value, is_default FROM party_electronic_address
             WHERE party_type = $1 AND party_id = $2 ORDER BY is_default DESC, created_at",
        )
        .bind(k)
        .bind(id)
        .fetch_all(&mut *conn)
        .await?,
        contacts: sqlx::query_as(
            "SELECT name, role, phone, email, is_default FROM party_contact
             WHERE party_type = $1 AND party_id = $2 ORDER BY is_default DESC, created_at",
        )
        .bind(k)
        .bind(id)
        .fetch_all(&mut *conn)
        .await?,
    })
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetInput {
    /// `company`, `customer` or `vendor`.
    party_type: Kind,
    /// The company, customer or vendor.
    #[schemars(extend("x-field-type" = { "kind": "entity_link", "entity_field": "party_type" }))]
    party_id: Uuid,
}

pub struct Get;

#[async_trait]
impl Command for Get {
    type Input = GetInput;
    type Output = PartyRecords;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "party.get",
            "Everything the company, a customer or a vendor has on record outside the books: \
             its legal particulars, addresses, tax identifiers, electronic addresses and \
             contacts. Takes the right to read that company, customer or vendor.",
        )
        .label("Party details")
        .resource("party")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: GetInput) -> Result<PartyRecords> {
        let kind = input.party_type.as_str();
        if owner(ctx, kind, input.party_id).await?.is_none() {
            return Err(Error::not_found(kind));
        }
        records(&mut *ctx.conn, input.party_type, input.party_id).await
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetInput {
    /// `company`, `customer` or `vendor`.
    party_type: Kind,
    /// The company, customer or vendor.
    #[schemars(extend("x-field-type" = { "kind": "entity_link", "entity_field": "party_type" }))]
    party_id: Uuid,
    /// The legal particulars, replacing what is on record. Left out: kept.
    profile: Option<Profile>,
    /// Every address, replacing those on record. Left out: kept.
    addresses: Option<Vec<AddressRecord>>,
    /// Every tax identifier, replacing those on record. Left out: kept.
    tax_ids: Option<Vec<TaxIdRecord>>,
    /// Every electronic address, replacing those on record. Left out: kept.
    electronic_addresses: Option<Vec<ElectronicAddressRecord>>,
    /// Every contact, replacing those on record. Left out: kept.
    contacts: Option<Vec<ContactRecord>>,
}

pub struct Set;

#[async_trait]
impl Command for Set {
    type Input = SetInput;
    type Output = PartyRecords;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "party.set",
            "Record the company's, a customer's or a vendor's particulars, addresses, tax \
             identifiers, electronic addresses or contacts. Each part given replaces what is on \
             record; a part left out is kept. At most one default address of each kind, one \
             default electronic address and one default contact; one tax identifier of each \
             scheme. Takes the right to change that company, customer or vendor.",
        )
        .label("Set party details")
        .resource("party")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: SetInput) -> Result<PartyRecords> {
        let kind = input.party_type;
        let k = kind.as_str();
        let id = input.party_id;
        // The owner's record as the caller may read it, and the owner's own
        // right to change it.
        let Some(record) = owner(ctx, k, id).await? else {
            return Err(Error::not_found(k));
        };
        ctx.actor
            .require_on(&format!("{k}:update"), kind.write_target(id), &record)?;
        ctx.touch(k, id);
        let conn = &mut *ctx.conn;
        if let Some(p) = input.profile {
            sqlx::query(
                "INSERT INTO party_profile (id, party_type, party_id, legal_name, trading_name,
                     legal_registration, legal_info, invoice_reference, payment_account_id)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 ON CONFLICT (party_type, party_id) DO UPDATE SET
                     legal_name = EXCLUDED.legal_name, trading_name = EXCLUDED.trading_name,
                     legal_registration = EXCLUDED.legal_registration,
                     legal_info = EXCLUDED.legal_info,
                     invoice_reference = EXCLUDED.invoice_reference,
                     payment_account_id = EXCLUDED.payment_account_id",
            )
            .bind(new_id())
            .bind(k)
            .bind(id)
            .bind(clean(p.legal_name))
            .bind(clean(p.trading_name))
            .bind(clean(p.legal_registration))
            .bind(clean(p.legal_info))
            .bind(clean(p.invoice_reference))
            .bind(p.payment_account_id)
            .execute(&mut *conn)
            .await?;
        }
        if let Some(list) = input.addresses {
            sqlx::query("DELETE FROM party_address WHERE party_type = $1 AND party_id = $2")
                .bind(k)
                .bind(id)
                .execute(&mut *conn)
                .await?;
            for a in list {
                sqlx::query(
                    "INSERT INTO party_address (id, party_type, party_id, kind, is_default,
                         attention, street, additional_street, city, postcode, region,
                         country_code)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
                )
                .bind(new_id())
                .bind(k)
                .bind(id)
                .bind(&a.kind)
                .bind(a.is_default)
                .bind(clean(a.attention))
                .bind(clean(a.street))
                .bind(clean(a.additional_street))
                .bind(clean(a.city))
                .bind(clean(a.postcode))
                .bind(clean(a.region))
                .bind(a.country_code.trim().to_uppercase())
                .execute(&mut *conn)
                .await?;
            }
        }
        if let Some(list) = input.tax_ids {
            sqlx::query("DELETE FROM party_tax_id WHERE party_type = $1 AND party_id = $2")
                .bind(k)
                .bind(id)
                .execute(&mut *conn)
                .await?;
            for t in list {
                sqlx::query(
                    "INSERT INTO party_tax_id (id, party_type, party_id, scheme, value,
                         country_code)
                     VALUES ($1, $2, $3, $4, $5, $6)",
                )
                .bind(new_id())
                .bind(k)
                .bind(id)
                .bind(&t.scheme)
                .bind(t.value.trim())
                .bind(clean(t.country_code).map(|c| c.to_uppercase()))
                .execute(&mut *conn)
                .await?;
            }
        }
        if let Some(list) = input.electronic_addresses {
            sqlx::query(
                "DELETE FROM party_electronic_address WHERE party_type = $1 AND party_id = $2",
            )
            .bind(k)
            .bind(id)
            .execute(&mut *conn)
            .await?;
            for e in list {
                sqlx::query(
                    "INSERT INTO party_electronic_address (id, party_type, party_id, scheme,
                         value, is_default)
                     VALUES ($1, $2, $3, $4, $5, $6)",
                )
                .bind(new_id())
                .bind(k)
                .bind(id)
                .bind(e.scheme.trim())
                .bind(e.value.trim())
                .bind(e.is_default)
                .execute(&mut *conn)
                .await?;
            }
        }
        if let Some(list) = input.contacts {
            sqlx::query("DELETE FROM party_contact WHERE party_type = $1 AND party_id = $2")
                .bind(k)
                .bind(id)
                .execute(&mut *conn)
                .await?;
            for c in list {
                sqlx::query(
                    "INSERT INTO party_contact (id, party_type, party_id, name, role, phone,
                         email, is_default)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                )
                .bind(new_id())
                .bind(k)
                .bind(id)
                .bind(clean(c.name))
                .bind(clean(c.role))
                .bind(clean(c.phone))
                .bind(clean(c.email))
                .bind(c.is_default)
                .execute(&mut *conn)
                .await?;
            }
        }
        records(&mut *ctx.conn, kind, id).await
    }
}

/// Text as it is kept: trimmed, and nothing when empty.
fn clean(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

// ------------------------------------------------------- document.parties

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartiesInput {
    /// The document's type, such as `sales_invoice` or `purchase_order`.
    doc_type: String,
    /// The document.
    id: Uuid,
}

/// A seller or a buyer as a document shows it. What the caller may not
/// read, or what is not set, is absent.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct PartyView {
    /// `company`, `customer` or `vendor`, and the record.
    pub kind: String,
    pub id: Uuid,
    pub name: Option<String>,
    pub street: Option<String>,
    pub additional_street: Option<String>,
    pub city: Option<String>,
    pub postcode: Option<String>,
    pub country_code: Option<String>,
    pub vat_id: Option<String>,
    pub tax_number: Option<String>,
    pub legal_registration: Option<String>,
    pub legal_info: Option<String>,
    pub contact_name: Option<String>,
    pub contact_phone: Option<String>,
    pub contact_email: Option<String>,
    pub electronic_address: Option<String>,
    /// For a customer: the reference it asks invoices to quote.
    pub buyer_reference: Option<String>,
}

impl From<PartyData> for PartyView {
    fn from(d: PartyData) -> Self {
        let address = d.address.clone().unwrap_or_default();
        let has_address = d.address.is_some();
        PartyView {
            kind: d.kind.clone(),
            id: d.id,
            name: d.legal_name.clone().or(d.name.clone()),
            street: address.street,
            additional_street: address.additional_street,
            city: address.city,
            postcode: address.postcode,
            country_code: has_address.then_some(address.country_code),
            vat_id: d.tax_id("eu_vat"),
            tax_number: d.tax_id("tax_number"),
            legal_registration: d.legal_registration,
            legal_info: d.legal_info,
            contact_name: d.contact_name,
            contact_phone: d.contact_phone,
            contact_email: d.contact_email,
            electronic_address: d.electronic_address.map(|(_, v)| v),
            buyer_reference: d.invoice_reference,
        }
    }
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct PartiesOutput {
    pub seller: Option<PartyView>,
    pub buyer: Option<PartyView>,
}

/// The company of a document: always named, as every printed document
/// names it; its particulars only to someone who may read the company.
async fn company_view(ctx: &mut Ctx<'_>, id: Uuid) -> Result<PartyView> {
    match load_readable(ctx, "company", id).await? {
        Some(data) => Ok(data.into()),
        None => {
            let name: Option<String> = sqlx::query_scalar("SELECT name FROM company WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *ctx.conn)
                .await?;
            Ok(PartyView {
                kind: "company".into(),
                id,
                name,
                ..PartyView::default()
            })
        }
    }
}

pub struct Parties;

#[async_trait]
impl Command for Parties {
    type Input = PartiesInput;
    type Output = PartiesOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "document.parties",
            "The seller and the buyer of a document, with the address, VAT identifier, \
             register entry and contact each has on record, as far as the caller may read \
             them: the company and the customer for a sales document, the vendor and the \
             company for a purchase document. Print templates receive it as `parties`. Takes \
             the right to read the document.",
        )
        .label("Parties")
        .resource("document")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: PartiesInput) -> Result<PartiesOutput> {
        let view = document::get(ctx, &input.doc_type, input.id).await?;
        let summary = &view.header.summary;
        let uuid = |field: &str| {
            summary[field]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
        };
        let company_id = view.header.company_id;
        let mut out = PartiesOutput::default();
        if let Some(customer) = uuid("customer_id") {
            out.seller = Some(company_view(ctx, company_id).await?);
            out.buyer = load_readable(ctx, "customer", customer)
                .await?
                .map(Into::into);
        } else if let Some(vendor) = uuid("vendor_id") {
            out.seller = load_readable(ctx, "vendor", vendor).await?.map(Into::into);
            out.buyer = Some(company_view(ctx, company_id).await?);
        } else {
            out.seller = Some(company_view(ctx, company_id).await?);
        }
        Ok(out)
    }
}
