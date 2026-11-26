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
    fn catalogues(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("de", include_str!("../i18n/de.json")),
            ("fr", include_str!("../i18n/fr.json")),
        ]
    }

    fn name(&self) -> &'static str {
        "party"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["gl", "sales", "purchasing"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "party",
                sql: include_str!("../migrations/0001_party.sql"),
            },
            Migration {
                version: 2,
                name: "document_party",
                sql: include_str!("../migrations/0002_document_party.sql"),
            },
            Migration {
                version: 3,
                name: "einvoice_format",
                sql: include_str!("../migrations/0003_einvoice_format.sql"),
            },
            Migration {
                version: 4,
                name: "language",
                sql: include_str!("../migrations/0004_language.sql"),
            },
            Migration {
                version: 5,
                name: "quebec_france_ids",
                sql: include_str!("../migrations/0005_quebec_france_ids.sql"),
            },
            Migration {
                version: 6,
                name: "us_ca_individuals",
                sql: include_str!("../migrations/0006_us_ca_individuals.sql"),
            },
        ]
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

#[derive(sqlx::FromRow)]
struct AddressRow {
    id: Uuid,
    #[sqlx(flatten)]
    address: Address,
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
    /// The party's address it was read from, while that is on record.
    #[serde(skip)]
    pub address_id: Option<Uuid>,
    /// `eu_vat`, `tax_number`, `us_ein`, `ca_bn`, `ca_qst`, `fr_siren` or
    /// `other`, and the value.
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
    load_at(conn, kind, id, Pick::Default("billing")).await
}

/// Which of the party's addresses to read.
#[derive(Debug, Clone, Copy)]
enum Pick {
    /// The default of this kind, else the first of it.
    Default(&'static str),
    /// This one, whatever its kind.
    Id(Uuid),
}

async fn load_at(conn: &mut PgConnection, kind: &str, id: Uuid, pick: Pick) -> Result<PartyData> {
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
    // The address named; else the default of the kind, else the first.
    let (address_kind, address_id) = match pick {
        Pick::Default(k) => (Some(k), None),
        Pick::Id(a) => (None, Some(a)),
    };
    let address: Option<(Uuid, Address)> = sqlx::query_as::<_, AddressRow>(
        "SELECT id, attention, street, additional_street, city, postcode, region, country_code
         FROM party_address
         WHERE party_type = $1 AND party_id = $2
           AND ($3::text IS NULL OR kind = $3) AND ($4::uuid IS NULL OR id = $4)
         ORDER BY is_default DESC, created_at LIMIT 1",
    )
    .bind(kind)
    .bind(id)
    .bind(address_kind)
    .bind(address_id)
    .fetch_optional(&mut *conn)
    .await?
    .map(|r| (r.id, r.address));
    data.address_id = address.as_ref().map(|(a, _)| *a);
    data.address = address.map(|(_, a)| a);
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

/// The address goods go to, as a sale keeps it: the shipping address
/// named, else the party's default shipping address, else the billing
/// address named, else its default billing address. `None` when the party
/// has no address. An address named that is no longer on record counts as
/// not named.
pub async fn ship_to(
    conn: &mut PgConnection,
    kind: &str,
    id: Uuid,
    shipping_address_id: Option<Uuid>,
    billing_address_id: Option<Uuid>,
) -> Result<Option<Address>> {
    Ok(sqlx::query_as(
        "SELECT attention, street, additional_street, city, postcode, region, country_code
         FROM party_address
         WHERE party_type = $1 AND party_id = $2
         ORDER BY id = $3 DESC NULLS LAST, kind = 'shipping' AND is_default DESC,
                  kind = 'shipping' DESC, id = $4 DESC NULLS LAST, is_default DESC, created_at
         LIMIT 1",
    )
    .bind(Kind::parse(kind)?.as_str())
    .bind(id)
    .bind(shipping_address_id)
    .bind(billing_address_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// Whether the party has a tax identifier of the scheme (`eu_vat`: a
/// business in the EU rather than a consumer).
pub async fn has_tax_id(
    conn: &mut PgConnection,
    kind: &str,
    id: Uuid,
    scheme: &str,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM party_tax_id
                        WHERE party_type = $1 AND party_id = $2 AND scheme = $3)",
    )
    .bind(Kind::parse(kind)?.as_str())
    .bind(id)
    .bind(scheme)
    .fetch_one(&mut *conn)
    .await?)
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

// ------------------------------------------------------ document snapshots

/// What a party is to a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Seller,
    Buyer,
    /// Where the goods go: a shipping address of the buyer.
    ShipTo,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Seller => "seller",
            Role::Buyer => "buyer",
            Role::ShipTo => "ship_to",
        }
    }

    /// The field of a document that names the address for this role.
    fn field(self) -> &'static str {
        match self {
            Role::ShipTo => "shipping_address_id",
            Role::Seller | Role::Buyer => "billing_address_id",
        }
    }
}

/// A field of a document that names one of its party's addresses, chosen
/// from what `party.get` lists of them. `party_type` is the kind of party,
/// `party_field` the document's field that holds it, `kind` the kind of
/// address offered (`billing` or `shipping`).
pub fn address_field(
    name: &str,
    label: &str,
    party_type: &str,
    party_field: &str,
    kind: &str,
    help: &str,
) -> erpcraft_kernel::registry::FieldMeta {
    use erpcraft_kernel::registry::{FieldMeta, FieldType};
    FieldMeta::new(
        name,
        label,
        FieldType::Lookup(
            erpcraft_kernel::Lookup::new("party.get", "addresses", "id")
                .display(&["street", "city", "postcode", "country_code"])
                .value("party_type", party_type)
                .field("party_id", party_field)
                .value("address_kind", kind),
        ),
    )
    .help(help)
}

/// Refuse an address that is not one of the party's. `field` is the field
/// of the document that names it. Any kind will do: a document may send
/// its invoice to a shipping address.
pub async fn check_address(
    conn: &mut PgConnection,
    kind: &str,
    party_id: Uuid,
    address_id: Uuid,
    field: &str,
) -> Result<()> {
    let found: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM party_address
                        WHERE id = $1 AND party_type = $2 AND party_id = $3)",
    )
    .bind(address_id)
    .bind(kind)
    .bind(party_id)
    .fetch_one(&mut *conn)
    .await?;
    if found {
        Ok(())
    } else {
        Err(Error::validation(format!("this is not an address of the {kind}")).with_field(field))
    }
}

/// Copy what the document shows of a party into `document_party`: its
/// name, particulars, tax identifiers, electronic address, contact, and
/// the address named (`address_id`) or else the default of the role's
/// kind (shipping for `ship_to`, billing otherwise). A posting command
/// calls it before it locks the document, after which the document child
/// guard keeps it as it is. Called again on a draft, it replaces the copy.
///
/// `ship_to` is written only when the party has a shipping address or
/// the document names one. An address named that is no longer on record
/// is refused, on the field that names it.
pub async fn snapshot(
    conn: &mut PgConnection,
    document_id: Uuid,
    role: Role,
    kind: &str,
    party_id: Uuid,
    address_id: Option<Uuid>,
) -> Result<()> {
    let pick = match (address_id, role) {
        (Some(a), _) => Pick::Id(a),
        (None, Role::ShipTo) => Pick::Default("shipping"),
        (None, _) => Pick::Default("billing"),
    };
    let d = load_at(conn, kind, party_id, pick).await?;
    if address_id.is_some() && d.address.is_none() {
        return Err(Error::validation(format!(
            "the address named is no longer on record for the {kind}"
        ))
        .with_field(role.field()));
    }
    if role == Role::ShipTo && d.address.is_none() {
        return Ok(());
    }
    let a = d.address.clone().unwrap_or_default();
    let has_address = d.address.is_some();
    let tax_ids = serde_json::to_value(&d.tax_ids).map_err(Error::internal)?;
    sqlx::query(
        "INSERT INTO document_party (document_id, role, party_type, party_id, address_id, name,
             legal_name, trading_name, legal_registration, legal_info, invoice_reference,
             payment_account_id, attention, street, additional_street, city, postcode, region,
             country_code, tax_ids, electronic_scheme, electronic_address, contact_name,
             contact_phone, contact_email)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18,
                 $19, $20, $21, $22, $23, $24, $25)
         ON CONFLICT (document_id, role) DO UPDATE SET
             party_type = EXCLUDED.party_type, party_id = EXCLUDED.party_id,
             address_id = EXCLUDED.address_id, name = EXCLUDED.name,
             legal_name = EXCLUDED.legal_name, trading_name = EXCLUDED.trading_name,
             legal_registration = EXCLUDED.legal_registration, legal_info = EXCLUDED.legal_info,
             invoice_reference = EXCLUDED.invoice_reference,
             payment_account_id = EXCLUDED.payment_account_id, attention = EXCLUDED.attention,
             street = EXCLUDED.street, additional_street = EXCLUDED.additional_street,
             city = EXCLUDED.city, postcode = EXCLUDED.postcode, region = EXCLUDED.region,
             country_code = EXCLUDED.country_code, tax_ids = EXCLUDED.tax_ids,
             electronic_scheme = EXCLUDED.electronic_scheme,
             electronic_address = EXCLUDED.electronic_address,
             contact_name = EXCLUDED.contact_name, contact_phone = EXCLUDED.contact_phone,
             contact_email = EXCLUDED.contact_email",
    )
    .bind(document_id)
    .bind(role.as_str())
    .bind(&d.kind)
    .bind(party_id)
    .bind(d.address_id)
    .bind(&d.name)
    .bind(&d.legal_name)
    .bind(&d.trading_name)
    .bind(&d.legal_registration)
    .bind(&d.legal_info)
    .bind(&d.invoice_reference)
    .bind(d.payment_account_id)
    .bind(&a.attention)
    .bind(&a.street)
    .bind(&a.additional_street)
    .bind(&a.city)
    .bind(&a.postcode)
    .bind(&a.region)
    .bind(has_address.then_some(&a.country_code))
    .bind(tax_ids)
    .bind(d.electronic_address.as_ref().map(|(s, _)| s))
    .bind(d.electronic_address.as_ref().map(|(_, v)| v))
    .bind(&d.contact_name)
    .bind(&d.contact_phone)
    .bind(&d.contact_email)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The parties of a sales document as it is posted: the company sells,
/// the customer buys at its billing address and receives the goods at its
/// shipping address, each the one named or else the default.
pub async fn snapshot_sale(
    conn: &mut PgConnection,
    document_id: Uuid,
    company_id: Uuid,
    customer_id: Uuid,
    billing_address_id: Option<Uuid>,
    shipping_address_id: Option<Uuid>,
) -> Result<()> {
    snapshot(conn, document_id, Role::Seller, "company", company_id, None).await?;
    snapshot(
        conn,
        document_id,
        Role::Buyer,
        "customer",
        customer_id,
        billing_address_id,
    )
    .await?;
    snapshot(
        conn,
        document_id,
        Role::ShipTo,
        "customer",
        customer_id,
        shipping_address_id,
    )
    .await
}

/// The parties of a purchase document as it is posted: the vendor sells,
/// at the address named or its default billing address, and the company
/// buys.
pub async fn snapshot_purchase(
    conn: &mut PgConnection,
    document_id: Uuid,
    company_id: Uuid,
    vendor_id: Uuid,
    billing_address_id: Option<Uuid>,
) -> Result<()> {
    snapshot(
        conn,
        document_id,
        Role::Seller,
        "vendor",
        vendor_id,
        billing_address_id,
    )
    .await?;
    snapshot(conn, document_id, Role::Buyer, "company", company_id, None).await
}

/// Give `to` the parties `from` kept: a credit note those of its invoice,
/// a return those of its order. False when `from` kept none, and nothing
/// is written.
pub async fn copy_parties(conn: &mut PgConnection, from: Uuid, to: Uuid) -> Result<bool> {
    sqlx::query("DELETE FROM document_party WHERE document_id = $1")
        .bind(to)
        .execute(&mut *conn)
        .await?;
    let copied = sqlx::query(
        "INSERT INTO document_party (document_id, role, party_type, party_id, address_id, name,
             legal_name, trading_name, legal_registration, legal_info, invoice_reference,
             payment_account_id, attention, street, additional_street, city, postcode, region,
             country_code, tax_ids, electronic_scheme, electronic_address, contact_name,
             contact_phone, contact_email)
         SELECT $2, role, party_type, party_id, address_id, name,
             legal_name, trading_name, legal_registration, legal_info, invoice_reference,
             payment_account_id, attention, street, additional_street, city, postcode, region,
             country_code, tax_ids, electronic_scheme, electronic_address, contact_name,
             contact_phone, contact_email
         FROM document_party WHERE document_id = $1",
    )
    .bind(from)
    .bind(to)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok(copied > 0)
}

#[derive(sqlx::FromRow)]
struct SnapshotRow {
    party_type: String,
    party_id: Uuid,
    address_id: Option<Uuid>,
    name: Option<String>,
    legal_name: Option<String>,
    trading_name: Option<String>,
    legal_registration: Option<String>,
    legal_info: Option<String>,
    invoice_reference: Option<String>,
    payment_account_id: Option<Uuid>,
    attention: Option<String>,
    street: Option<String>,
    additional_street: Option<String>,
    city: Option<String>,
    postcode: Option<String>,
    region: Option<String>,
    country_code: Option<String>,
    tax_ids: sqlx::types::Json<Vec<(String, String)>>,
    electronic_scheme: Option<String>,
    electronic_address: Option<String>,
    contact_name: Option<String>,
    contact_phone: Option<String>,
    contact_email: Option<String>,
}

/// What a document kept of the party in `role` when it was posted;
/// nothing for a document that kept none (a draft, or one posted before
/// documents kept their parties).
pub async fn document_data(
    conn: &mut PgConnection,
    document_id: Uuid,
    role: Role,
) -> Result<Option<PartyData>> {
    let row: Option<SnapshotRow> = sqlx::query_as(
        "SELECT party_type, party_id, address_id, name, legal_name, trading_name,
                legal_registration, legal_info, invoice_reference, payment_account_id,
                attention, street, additional_street, city, postcode, region, country_code,
                tax_ids, electronic_scheme, electronic_address, contact_name, contact_phone,
                contact_email
         FROM document_party WHERE document_id = $1 AND role = $2",
    )
    .bind(document_id)
    .bind(role.as_str())
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| PartyData {
        kind: r.party_type,
        id: r.party_id,
        name: r.name,
        legal_name: r.legal_name,
        trading_name: r.trading_name,
        legal_registration: r.legal_registration,
        legal_info: r.legal_info,
        invoice_reference: r.invoice_reference,
        payment_account_id: r.payment_account_id,
        address: r.country_code.map(|country_code| Address {
            attention: r.attention,
            street: r.street,
            additional_street: r.additional_street,
            city: r.city,
            postcode: r.postcode,
            region: r.region,
            country_code,
        }),
        address_id: r.address_id,
        tax_ids: r.tax_ids.0,
        electronic_address: r.electronic_scheme.zip(r.electronic_address),
        contact_name: r.contact_name,
        contact_phone: r.contact_phone,
        contact_email: r.contact_email,
    }))
}

/// The party in `role` as the document shows it: what it kept when it was
/// posted, else the party's data now.
///
/// What the document kept is never replaced. What it did not have when it
/// was posted (no address on record then, no VAT identifier, no contact)
/// is filled in from the party's data now, so that a document posted
/// before the data was entered can still be printed and sent.
pub async fn load_for_document(
    conn: &mut PgConnection,
    document_id: Uuid,
    role: Role,
    kind: &str,
    id: Uuid,
) -> Result<PartyData> {
    let kept = match document_data(conn, document_id, role).await? {
        Some(d) if d.kind == kind && d.id == id => d,
        _ => return load(conn, kind, id).await,
    };
    let now = load(conn, kind, id).await?;
    Ok(fill_gaps(kept, now))
}

/// `kept`, with what it lacks taken from `now`.
fn fill_gaps(mut kept: PartyData, now: PartyData) -> PartyData {
    fn fill<T>(slot: &mut Option<T>, now: Option<T>) {
        if slot.is_none() {
            *slot = now;
        }
    }
    fill(&mut kept.name, now.name);
    fill(&mut kept.legal_name, now.legal_name);
    fill(&mut kept.trading_name, now.trading_name);
    fill(&mut kept.legal_registration, now.legal_registration);
    fill(&mut kept.legal_info, now.legal_info);
    fill(&mut kept.invoice_reference, now.invoice_reference);
    fill(&mut kept.payment_account_id, now.payment_account_id);
    if kept.address.is_none() {
        kept.address = now.address;
        kept.address_id = now.address_id;
    }
    for (scheme, value) in now.tax_ids {
        if !kept.tax_ids.iter().any(|(s, _)| *s == scheme) {
            kept.tax_ids.push((scheme, value));
        }
    }
    kept.tax_ids.sort();
    fill(&mut kept.electronic_address, now.electronic_address);
    if kept.contact_name.is_none() && kept.contact_phone.is_none() && kept.contact_email.is_none() {
        kept.contact_name = now.contact_name;
        kept.contact_phone = now.contact_phone;
        kept.contact_email = now.contact_email;
    }
    kept
}

/// How a customer's invoices are sent by email: what its profile says,
/// else XRechnung in UBL when it has a Leitweg-ID (an electronic address of
/// scheme 0204, or an invoice reference shaped like one) or an electronic
/// address other than an email, else a PDF.
/// The language a party is written to in: its own, else (for a customer
/// or vendor) its company's, else English.
pub async fn language(
    conn: &mut PgConnection,
    company_id: Uuid,
    party: Option<(&str, Uuid)>,
) -> Result<String> {
    let found: Option<String> = sqlx::query_scalar(
        "SELECT language FROM party_profile
         WHERE language IS NOT NULL
           AND ((party_type = $1 AND party_id = $2) OR (party_type = 'company' AND party_id = $3))
         ORDER BY party_type = 'company' LIMIT 1",
    )
    .bind(party.map(|p| p.0))
    .bind(party.map(|p| p.1))
    .bind(company_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(found.unwrap_or_else(|| "en".into()))
}

pub async fn einvoice_format(conn: &mut PgConnection, customer_id: Uuid) -> Result<String> {
    let (set, reference): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT einvoice_format, invoice_reference FROM party_profile
         WHERE party_type = 'customer' AND party_id = $1",
    )
    .bind(customer_id)
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or((None, None));
    if let Some(set) = set {
        return Ok(set);
    }
    let electronic: Vec<String> = sqlx::query_scalar(
        "SELECT scheme FROM party_electronic_address
         WHERE party_type = 'customer' AND party_id = $1",
    )
    .bind(customer_id)
    .fetch_all(&mut *conn)
    .await?;
    let leitweg = reference.as_deref().is_some_and(is_leitweg_id);
    if leitweg || electronic.iter().any(|s| s != "EM") {
        Ok("xrechnung_ubl".into())
    } else {
        Ok("pdf".into())
    }
}

/// Whether `text` is shaped like a Leitweg-ID: a coarse address of two to
/// twelve digits, an optional fine address, and two check digits, joined
/// by hyphens ("04011000-12345-03").
pub fn is_leitweg_id(text: &str) -> bool {
    let parts: Vec<&str> = text.trim().split('-').collect();
    let coarse = |p: &str| (2..=12).contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit());
    let check = |p: &str| p.len() == 2 && p.chars().all(|c| c.is_ascii_digit());
    match parts.as_slice() {
        [c, k] => coarse(c) && check(k),
        [c, fine, k] => {
            coarse(c)
                && (1..=30).contains(&fine.len())
                && fine.chars().all(|c| c.is_ascii_alphanumeric())
                && check(k)
        }
        _ => false,
    }
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
    /// For a customer: how its invoices are sent by email: `pdf`,
    /// `zugferd` (a ZUGFeRD / Factur-X PDF), `xrechnung_ubl` or
    /// `xrechnung_cii` (the XML alone). Left out: a Leitweg-ID or an
    /// electronic address other than an email means XRechnung in UBL,
    /// anything else a PDF.
    #[serde(default)]
    #[sqlx(default)]
    pub einvoice_format: Option<String>,
    /// The language the party is written to in, ISO 639-1 (`de`, `en`):
    /// its printed documents. Left out on a customer or vendor: the
    /// company's; on the company: English.
    #[serde(default)]
    #[sqlx(default)]
    pub language: Option<String>,
}

/// A postal address of the party.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct AddressRecord {
    /// The address on record this one is, when it was read before. An
    /// address keeps its id while it is given back with it, so that the
    /// documents that name it still find it; one left out is removed.
    #[serde(default)]
    pub id: Option<Uuid>,
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
    /// Steuernummer), `us_ein`, `ca_bn` (the Canadian business number, which
    /// carries the GST/HST account), `ca_qst` (the Quebec sales tax number),
    /// `ca_sin` (a Canadian Social Insurance Number), `us_ssn` (a US Social
    /// Security Number), `fr_siren` (nine digits) or `other`. One of each.
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

async fn records(
    conn: &mut PgConnection,
    kind: Kind,
    id: Uuid,
    address_kind: Option<&str>,
) -> Result<PartyRecords> {
    let k = kind.as_str();
    Ok(PartyRecords {
        party_type: k.into(),
        party_id: id,
        profile: sqlx::query_as(
            "SELECT legal_name, trading_name, legal_registration, legal_info, invoice_reference,
                    payment_account_id, einvoice_format, language
             FROM party_profile WHERE party_type = $1 AND party_id = $2",
        )
        .bind(k)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?,
        addresses: sqlx::query_as(
            "SELECT id, kind, is_default, attention, street, additional_street, city, postcode,
                    region, country_code
             FROM party_address WHERE party_type = $1 AND party_id = $2
               AND ($3::text IS NULL OR kind = $3)
             ORDER BY kind, is_default DESC, created_at",
        )
        .bind(k)
        .bind(id)
        .bind(address_kind)
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
    /// Only the addresses of this kind, `billing` or `shipping`: what a
    /// document offers to choose from.
    #[serde(default)]
    address_kind: Option<String>,
    /// As a field's lookup: only addresses whose lines, city, post code or
    /// country contain this text, ignoring case.
    #[serde(default)]
    q: Option<String>,
    /// As a field's lookup: at most this many addresses.
    #[serde(default)]
    limit: Option<i64>,
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
        let mut found = records(
            &mut *ctx.conn,
            input.party_type,
            input.party_id,
            input.address_kind.as_deref(),
        )
        .await?;
        if let Some(q) = input.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            let q = q.to_lowercase();
            found.addresses.retain(|a| {
                [
                    &a.attention,
                    &a.street,
                    &a.additional_street,
                    &a.city,
                    &a.postcode,
                    &a.region,
                    &Some(a.country_code.clone()),
                ]
                .into_iter()
                .flatten()
                .any(|text| text.to_lowercase().contains(&q))
            });
        }
        if let Some(limit) = input.limit {
            found.addresses.truncate(limit.clamp(0, 1000) as usize);
        }
        Ok(found)
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
                     legal_registration, legal_info, invoice_reference, payment_account_id,
                     einvoice_format, language)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                 ON CONFLICT (party_type, party_id) DO UPDATE SET
                     legal_name = EXCLUDED.legal_name, trading_name = EXCLUDED.trading_name,
                     legal_registration = EXCLUDED.legal_registration,
                     legal_info = EXCLUDED.legal_info,
                     invoice_reference = EXCLUDED.invoice_reference,
                     payment_account_id = EXCLUDED.payment_account_id,
                     einvoice_format = EXCLUDED.einvoice_format,
                     language = EXCLUDED.language",
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
            .bind(clean(p.einvoice_format))
            .bind(clean(p.language).map(|l| l.to_lowercase()))
            .execute(&mut *conn)
            .await?;
        }
        if let Some(list) = input.addresses {
            // Addresses given back with their id keep it; the others on
            // record go. Defaults are cleared first so that moving the
            // default from one address to another does not meet itself.
            let kept: Vec<Uuid> = list.iter().filter_map(|a| a.id).collect();
            sqlx::query(
                "DELETE FROM party_address WHERE party_type = $1 AND party_id = $2
                   AND NOT (id = ANY($3))",
            )
            .bind(k)
            .bind(id)
            .bind(&kept)
            .execute(&mut *conn)
            .await?;
            sqlx::query(
                "UPDATE party_address SET is_default = false
                 WHERE party_type = $1 AND party_id = $2 AND is_default",
            )
            .bind(k)
            .bind(id)
            .execute(&mut *conn)
            .await?;
            let mut seen = std::collections::HashSet::new();
            for (index, a) in list.into_iter().enumerate() {
                if let Some(address) = a.id
                    && !seen.insert(address)
                {
                    return Err(Error::line(
                        "addresses",
                        index + 1,
                        Some("id"),
                        "the same address is given twice",
                    ));
                }
                let done = sqlx::query(
                    "UPDATE party_address SET kind = $4, is_default = $5, attention = $6,
                         street = $7, additional_street = $8, city = $9, postcode = $10,
                         region = $11, country_code = $12
                     WHERE id = $1 AND party_type = $2 AND party_id = $3",
                )
                .bind(a.id)
                .bind(k)
                .bind(id)
                .bind(&a.kind)
                .bind(a.is_default)
                .bind(clean(a.attention.clone()))
                .bind(clean(a.street.clone()))
                .bind(clean(a.additional_street.clone()))
                .bind(clean(a.city.clone()))
                .bind(clean(a.postcode.clone()))
                .bind(clean(a.region.clone()))
                .bind(a.country_code.trim().to_uppercase())
                .execute(&mut *conn)
                .await?
                .rows_affected();
                if done > 0 {
                    continue;
                }
                if a.id.is_some() {
                    return Err(Error::line(
                        "addresses",
                        index + 1,
                        Some("id"),
                        "this is not an address of this party; leave the id out for a new one",
                    ));
                }
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
        records(&mut *ctx.conn, kind, id, None).await
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
    pub region: Option<String>,
    pub country_code: Option<String>,
    pub vat_id: Option<String>,
    pub tax_number: Option<String>,
    pub ca_bn: Option<String>,
    pub ca_qst: Option<String>,
    pub siren: Option<String>,
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
            region: address.region,
            country_code: has_address.then_some(address.country_code),
            vat_id: d.tax_id("eu_vat"),
            tax_number: d.tax_id("tax_number"),
            ca_bn: d.tax_id("ca_bn"),
            ca_qst: d.tax_id("ca_qst"),
            siren: d.tax_id("fr_siren"),
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
    /// Where the goods go, when the document kept a shipping address of
    /// its buyer. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ship_to: Option<PartyView>,
}

/// A party of a document as the caller may see it: what the document kept
/// when it was posted, else the party's data now; nothing if the caller
/// may not read the party's record.
async fn party_view(
    ctx: &mut Ctx<'_>,
    document_id: Uuid,
    role: Role,
    kind: &str,
    id: Uuid,
) -> Result<Option<PartyView>> {
    if owner(ctx, kind, id).await?.is_none() {
        return Ok(None);
    }
    Ok(Some(
        load_for_document(&mut *ctx.conn, document_id, role, kind, id)
            .await?
            .into(),
    ))
}

/// The company of a document: always named, as every printed document
/// names it; its particulars only to someone who may read the company.
async fn company_view(
    ctx: &mut Ctx<'_>,
    document_id: Uuid,
    role: Role,
    id: Uuid,
) -> Result<PartyView> {
    match party_view(ctx, document_id, role, "company", id).await? {
        Some(view) => Ok(view),
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
        let doc = input.id;
        let mut out = PartiesOutput::default();
        // A document about a party of either kind, such as a payment, names
        // it as `party_type` and `party_id`.
        let party = |kind: &str| {
            (summary["party_type"].as_str() == Some(kind))
                .then(|| uuid("party_id"))
                .flatten()
        };
        if let Some(customer) = uuid("customer_id").or_else(|| party("customer")) {
            out.seller = Some(company_view(ctx, doc, Role::Seller, company_id).await?);
            out.buyer = party_view(ctx, doc, Role::Buyer, "customer", customer).await?;
            if out.buyer.is_some()
                && let Some(d) = document_data(&mut *ctx.conn, doc, Role::ShipTo).await?
                && d.id == customer
            {
                out.ship_to = Some(d.into());
            }
        } else if let Some(vendor) = uuid("vendor_id").or_else(|| party("vendor")) {
            out.seller = party_view(ctx, doc, Role::Seller, "vendor", vendor).await?;
            out.buyer = Some(company_view(ctx, doc, Role::Buyer, company_id).await?);
        } else {
            out.seller = Some(company_view(ctx, doc, Role::Seller, company_id).await?);
        }
        Ok(out)
    }
}
