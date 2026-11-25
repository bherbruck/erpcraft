//! Tax by destination: the code a line starts with chosen by where the
//! goods go, from data (areas, registrations, certificates), and the
//! reports that read the place back. See "Tax by destination" in
//! ARCHITECTURE.md.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, PgConnection, Result, async_trait,
    company, new_id, schemars,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Where goods go, or where a buyer is: as an address gives it.
#[derive(Debug, Clone, Default)]
pub struct Place {
    pub country_code: String,
    /// The subdivision as ISO 3166-2 writes it without the country.
    pub region: Option<String>,
    pub postcode: Option<String>,
    pub city: Option<String>,
}

/// Who a line is for.
#[derive(Debug, Clone)]
pub enum Side {
    /// A sale to a customer, who is a consumer when it has no VAT
    /// identifier.
    Sale { customer_id: Uuid, consumer: bool },
    /// A purchase from a vendor, at its billing address (its country and
    /// region) when it has one.
    Purchase {
        vendor_id: Uuid,
        vendor_at: Option<(String, Option<String>)>,
    },
}

/// What [`propose`] chooses a line's code from.
#[derive(Debug, Clone)]
pub struct Proposal<'a> {
    pub company_id: Uuid,
    /// The document's date: areas, registrations and certificates valid
    /// on it count.
    pub date: NaiveDate,
    pub side: Side,
    pub item_id: Option<Uuid>,
    pub charge_id: Option<Uuid>,
    /// Where the goods go on a sale; where the company is on a purchase.
    pub place: Option<&'a Place>,
}

/// The tax code an invoice line starts with when it names none.
///
/// On a sale: the customer's rule, else a valid exemption certificate of
/// the customer for the place, else the item's rule, else the place's code
/// (the components the company may charge there), else the charge's own
/// code, else the company's default. On a purchase: the vendor's rule,
/// else the item's, else the place's code (the components the vendor
/// charges, or what the company accounts for instead), else the charge's,
/// else the default. `None` when nothing says.
pub async fn propose(conn: &mut PgConnection, p: &Proposal<'_>) -> Result<Option<Uuid>> {
    let (party_type, party_id) = match &p.side {
        Side::Sale { customer_id, .. } => ("customer", *customer_id),
        Side::Purchase { vendor_id, .. } => ("vendor", *vendor_id),
    };
    let rules: Vec<(Option<String>, Uuid)> = sqlx::query_as(
        "SELECT scope_type, tax_code_id FROM tax_rule
         WHERE company_id = $1
           AND (scope_type IS NULL
                OR (scope_type = $2 AND scope_id = $3)
                OR (scope_type = 'item' AND scope_id = $4))",
    )
    .bind(p.company_id)
    .bind(party_type)
    .bind(party_id)
    .bind(p.item_id)
    .fetch_all(&mut *conn)
    .await?;
    let rule = |scope: Option<&str>| {
        rules
            .iter()
            .find(|(t, _)| t.as_deref() == scope)
            .map(|(_, code)| *code)
    };
    if let Some(code) = rule(Some(party_type)) {
        return Ok(Some(code));
    }
    if let (Side::Sale { customer_id, .. }, Some(place)) = (&p.side, p.place)
        && let Some((_, code)) =
            certificate(conn, p.company_id, *customer_id, place, p.date, None).await?
    {
        return Ok(Some(code));
    }
    if p.item_id.is_some()
        && let Some(code) = rule(Some("item"))
    {
        return Ok(Some(code));
    }
    if let Some(place) = p.place
        && let Some(code) = place_code(conn, p, place).await?
    {
        return Ok(Some(code));
    }
    if let Some(charge) = p.charge_id {
        let own: Option<Uuid> = sqlx::query_scalar("SELECT tax_code_id FROM charge WHERE id = $1")
            .bind(charge)
            .fetch_optional(&mut *conn)
            .await?
            .flatten();
        if own.is_some() {
            return Ok(own);
        }
    }
    Ok(rule(None))
}

/// The customer's exemption certificate for the place on `date`: one for
/// its region before one for the whole country. With `code`, only one that
/// gives that code. The certificate and the code it gives.
async fn certificate(
    conn: &mut PgConnection,
    company_id: Uuid,
    customer_id: Uuid,
    place: &Place,
    date: NaiveDate,
    code: Option<Uuid>,
) -> Result<Option<(Uuid, Uuid)>> {
    Ok(sqlx::query_as(
        "SELECT id, tax_code_id FROM tax_exemption
         WHERE company_id = $1 AND customer_id = $2 AND country_code = $3
           AND (region IS NULL OR upper(region) = upper($4))
           AND valid_from <= $5 AND (expires_on IS NULL OR expires_on >= $5)
           AND ($6::uuid IS NULL OR tax_code_id = $6)
         ORDER BY region IS NULL, valid_from DESC LIMIT 1",
    )
    .bind(company_id)
    .bind(customer_id)
    .bind(&place.country_code)
    .bind(&place.region)
    .bind(date)
    .bind(code)
    .fetch_optional(&mut *conn)
    .await?)
}

/// A post code as areas compare it: upper case, without spaces.
fn normal_postcode(postcode: &str) -> String {
    postcode
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_uppercase)
        .collect()
}

/// The code of the most specific area that contains the place on `date`.
async fn area_code(
    conn: &mut PgConnection,
    place: &Place,
    date: NaiveDate,
) -> Result<Option<Uuid>> {
    let postcode = place.postcode.as_deref().map(normal_postcode);
    Ok(sqlx::query_scalar(
        "SELECT tax_code_id FROM tax_area
         WHERE country_code = $1
           AND (region IS NULL OR upper(region) = upper($2))
           AND (city IS NULL OR lower(city) = lower($3))
           AND (postcode_from IS NULL
                OR left($4, length(postcode_from)) BETWEEN postcode_from AND postcode_to)
           AND valid_from <= $5 AND (valid_to IS NULL OR valid_to >= $5)
         ORDER BY (postcode_from IS NOT NULL AND city IS NOT NULL) DESC,
                  postcode_from IS NOT NULL DESC, city IS NOT NULL DESC,
                  region IS NOT NULL DESC, length(postcode_from) DESC NULLS LAST,
                  valid_from DESC
         LIMIT 1",
    )
    .bind(&place.country_code)
    .bind(&place.region)
    .bind(&place.city)
    .bind(postcode)
    .bind(date)
    .fetch_optional(&mut *conn)
    .await?)
}

/// One jurisdiction of an area's code.
#[derive(Debug, Clone, sqlx::FromRow)]
struct Jurisdiction {
    id: Uuid,
    country_code: Option<String>,
    region: Option<String>,
    self_assessed_code_id: Option<Uuid>,
}

fn same_region(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

/// A registration of the company valid on the date.
#[derive(Debug, Clone, sqlx::FromRow)]
struct Registration {
    country_code: String,
    region: Option<String>,
    scheme: String,
    consumers_only: bool,
    threshold: Option<Decimal>,
}

/// The place's code as far as it applies: see [`propose`].
async fn place_code(
    conn: &mut PgConnection,
    p: &Proposal<'_>,
    place: &Place,
) -> Result<Option<Uuid>> {
    let Some(area) = area_code(conn, place, p.date).await? else {
        return Ok(None);
    };
    let mut parts: Vec<Jurisdiction> = sqlx::query_as(
        "SELECT c.id, c.country_code, c.region, c.self_assessed_code_id
         FROM tax_code_component g JOIN tax_code c ON c.id = g.component_id
         WHERE g.group_id = $1 ORDER BY g.position",
    )
    .bind(area)
    .fetch_all(&mut *conn)
    .await?;
    if parts.is_empty() {
        parts = sqlx::query_as(
            "SELECT id, country_code, region, self_assessed_code_id FROM tax_code WHERE id = $1",
        )
        .bind(area)
        .fetch_all(&mut *conn)
        .await?;
    }
    let mut kept = vec![];
    match &p.side {
        Side::Sale { consumer, .. } => {
            let registrations: Vec<Registration> = sqlx::query_as(
                "SELECT country_code, region, scheme, consumers_only, threshold
                 FROM tax_registration
                 WHERE company_id = $1 AND valid_from <= $2 AND (valid_to IS NULL OR valid_to >= $2)",
            )
            .bind(p.company_id)
            .bind(p.date)
            .fetch_all(&mut *conn)
            .await?;
            let mut past_threshold: BTreeMap<String, bool> = BTreeMap::new();
            for part in &parts {
                let Some(country) = &part.country_code else {
                    kept.push(part.id);
                    continue;
                };
                let mut charged = registrations.iter().any(|r| {
                    r.scheme == "local"
                        && r.country_code == *country
                        && same_region(r.region.as_deref(), part.region.as_deref())
                        && (*consumer || !r.consumers_only)
                });
                if !charged && *consumer {
                    for oss in registrations.iter().filter(|r| r.scheme == "oss") {
                        if oss.country_code == *country
                            || !in_union_with(conn, &oss.country_code, country).await?
                        {
                            continue;
                        }
                        let past = match past_threshold.get(&oss.country_code) {
                            Some(past) => *past,
                            None => {
                                let past = beyond_threshold(conn, p, oss).await?;
                                past_threshold.insert(oss.country_code.clone(), past);
                                past
                            }
                        };
                        if past {
                            charged = true;
                            break;
                        }
                    }
                }
                if charged {
                    kept.push(part.id);
                }
            }
        }
        Side::Purchase { vendor_at, .. } => {
            for part in &parts {
                let charges = match (&part.country_code, vendor_at) {
                    (None, _) | (_, None) => true,
                    (Some(country), Some((vendor_country, vendor_region))) => {
                        country == vendor_country
                            && (part.region.is_none()
                                || same_region(part.region.as_deref(), vendor_region.as_deref()))
                    }
                };
                if charges {
                    kept.push(part.id);
                } else if let Some(instead) = part.self_assessed_code_id {
                    kept.push(instead);
                }
            }
        }
    }
    let unchanged = kept.len() == parts.len() && kept.iter().zip(&parts).all(|(k, p)| *k == p.id);
    match kept.len() {
        0 => Ok(None),
        _ if unchanged => Ok(Some(area)),
        1 => Ok(Some(kept[0])),
        _ => group_of(conn, &kept).await.map(Some),
    }
}

/// Whether `country` is a member of a union `home` is a member of.
async fn in_union_with(conn: &mut PgConnection, home: &str, country: &str) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM tax_union_member a JOIN tax_union_member b
                        ON b.union_code = a.union_code
                        WHERE a.country_code = $1 AND b.country_code = $2)",
    )
    .bind(home)
    .bind(country)
    .fetch_one(&mut *conn)
    .await?)
}

/// Whether the company's distance sales to consumers in other member
/// states of the union, in the year of the proposal or the one before,
/// came to more than the registration's threshold (none: taxed at
/// destination from the first sale).
async fn beyond_threshold(
    conn: &mut PgConnection,
    p: &Proposal<'_>,
    oss: &Registration,
) -> Result<bool> {
    let Some(threshold) = oss.threshold.filter(|t| !t.is_zero()) else {
        return Ok(true);
    };
    let year = p.date.year();
    let most: Decimal = distance_sales(conn, p.company_id, &oss.country_code, year - 1, year)
        .await?
        .into_values()
        .max()
        .unwrap_or_default();
    Ok(most > threshold)
}

/// The bases of sales to consumers in other member states of `home`'s
/// union, by calendar year from `first` to `last`.
pub async fn distance_sales(
    conn: &mut PgConnection,
    company_id: Uuid,
    home: &str,
    first: i32,
    last: i32,
) -> Result<BTreeMap<i32, Decimal>> {
    // One base per line: the components of a group that are not
    // compounding were all charged on it.
    let rows: Vec<(i32, Decimal)> = sqlx::query_as(
        "SELECT y, sum(line_base) FROM (
             SELECT extract(year FROM e.posting_date)::int AS y, avg(e.base) AS line_base
             FROM tax_entry e
             WHERE e.company_id = $1 AND e.direction = 'collected' AND e.consumer
               AND NOT e.compound AND NOT e.self_assessed AND e.country_code <> $2
               AND e.country_code IN (SELECT b.country_code FROM tax_union_member a
                                      JOIN tax_union_member b ON b.union_code = a.union_code
                                      WHERE a.country_code = $2)
               AND e.posting_date BETWEEN make_date($3, 1, 1) AND make_date($4, 12, 31)
             GROUP BY e.posting_id, e.posting_date, COALESCE(e.source_line_id, e.id)
         ) l GROUP BY y",
    )
    .bind(company_id)
    .bind(home)
    .bind(first)
    .bind(last)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().collect())
}

/// The group of exactly these components, in this order: one that exists
/// (active and not accounted for by the buyer as a whole), else a new one
/// coded by its parts (`A+B`) and named by them.
async fn group_of(conn: &mut PgConnection, components: &[Uuid]) -> Result<Uuid> {
    let found: Option<Uuid> = sqlx::query_scalar(
        "SELECT g.group_id FROM tax_code_component g JOIN tax_code t ON t.id = g.group_id
         WHERE NOT t.self_assessed
         GROUP BY g.group_id, t.active, t.code
         HAVING array_agg(g.component_id ORDER BY g.position) = $1::uuid[]
         ORDER BY t.active DESC, t.code LIMIT 1",
    )
    .bind(components)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(group) = found {
        return Ok(group);
    }
    let parts: Vec<(Uuid, String, String, Decimal)> = sqlx::query_as(
        "SELECT c.id, c.code, c.name, c.rate FROM unnest($1::uuid[]) WITH ORDINALITY AS u(id, n)
         JOIN tax_code c ON c.id = u.id ORDER BY u.n",
    )
    .bind(components)
    .fetch_all(&mut *conn)
    .await?;
    let code = parts
        .iter()
        .map(|p| p.1.as_str())
        .collect::<Vec<_>>()
        .join("+");
    let name = parts
        .iter()
        .map(|p| p.2.as_str())
        .collect::<Vec<_>>()
        .join(" + ");
    let rate: Decimal = parts.iter().map(|p| p.3).sum();
    let id = new_id();
    let inserted = sqlx::query(
        "INSERT INTO tax_code (id, code, name, rate) VALUES ($1, $2, $3, $4)
         ON CONFLICT (code) DO NOTHING",
    )
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(rate.min(Decimal::ONE_HUNDRED))
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if inserted == 0 {
        // Another transaction made it meanwhile, or the code is taken by
        // something else.
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT t.id FROM tax_code t WHERE t.code = $2
               AND (SELECT array_agg(g.component_id ORDER BY g.position)
                    FROM tax_code_component g WHERE g.group_id = t.id) = $1::uuid[]",
        )
        .bind(components)
        .bind(&code)
        .fetch_optional(&mut *conn)
        .await?;
        return existing.ok_or_else(|| {
            Error::state(format!(
                "tax code {code} exists and is not the group of {code}'s components"
            ))
        });
    }
    for (position, component) in components.iter().enumerate() {
        sqlx::query(
            "INSERT INTO tax_code_component (id, group_id, component_id, position)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(new_id())
        .bind(id)
        .bind(component)
        .bind(position as i32 + 1)
        .execute(&mut *conn)
        .await?;
    }
    Ok(id)
}

/// What a posting's entries are for, read from the parties the document
/// kept: the place (`ship_to`, else `buyer`), whether the buyer is a
/// customer without a VAT identifier, and the customer.
#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub(crate) struct Kept {
    pub country_code: Option<String>,
    pub region: Option<String>,
    pub consumer: Option<bool>,
    pub customer_id: Option<Uuid>,
}

pub(crate) async fn kept(conn: &mut PgConnection, posting_id: Uuid) -> Result<Kept> {
    Ok(sqlx::query_as(
        "SELECT d.country_code, d.region,
                CASE WHEN b.party_type = 'customer' THEN NOT EXISTS (
                    SELECT 1 FROM jsonb_array_elements(b.tax_ids) t WHERE t->>0 = 'eu_vat')
                END AS consumer,
                CASE WHEN b.party_type = 'customer' THEN b.party_id END AS customer_id
         FROM posting p
         JOIN document_party d ON d.document_id = p.document_id AND d.role IN ('ship_to', 'buyer')
         LEFT JOIN document_party b ON b.document_id = p.document_id AND b.role = 'buyer'
         WHERE p.id = $1
         ORDER BY d.role = 'ship_to' DESC LIMIT 1",
    )
    .bind(posting_id)
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or_default())
}

/// The certificate a sale at `code` is exempt under, on the posting date,
/// for the place the document kept; refused when there is none.
pub(crate) async fn exemption_for(
    conn: &mut PgConnection,
    company_id: Uuid,
    kept: &Kept,
    code: Uuid,
    date: NaiveDate,
) -> Result<Uuid> {
    let refuse = |why: &str| {
        Error::validation(format!(
            "a line taxed with a code for exemption certificates needs {why}"
        ))
    };
    let (Some(customer), Some(country)) = (kept.customer_id, &kept.country_code) else {
        return Err(refuse("a customer and the address the goods go to"));
    };
    let place = Place {
        country_code: country.clone(),
        region: kept.region.clone(),
        ..Place::default()
    };
    match certificate(conn, company_id, customer, &place, date, Some(code)).await? {
        Some((id, _)) => Ok(id),
        None => {
            let code: String = sqlx::query_scalar("SELECT code FROM tax_code WHERE id = $1")
                .bind(code)
                .fetch_one(&mut *conn)
                .await?;
            let at = match &place.region {
                Some(region) => format!("{country}-{region}"),
                None => country.clone(),
            };
            Err(refuse(&format!(
                "an exemption certificate of the customer valid for {at} on {date} that gives {code}"
            )))
        }
    }
}

// ------------------------------------------------------------- import

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportInput {
    /// The rate table, CSV with a header: one row per jurisdiction of an
    /// area. Required columns `country`, `code`, `rate`; optional `region`,
    /// `postcode_from`, `postcode_to`, `city`, `valid_from`, `valid_to`,
    /// `name`, `authority`, `registration` (`region`, the default, or
    /// `country`) and `source`.
    csv: String,
    /// With both accounts: give each code that this company has no
    /// accounts for these.
    company_id: Option<Uuid>,
    /// With the company: the account for tax collected on the new codes.
    collected_account_id: Option<Uuid>,
    /// With the company: the account for tax paid on the new codes.
    paid_account_id: Option<Uuid>,
}

#[derive(Serialize, JsonSchema, Default)]
pub(crate) struct ImportOutput {
    codes_created: u32,
    groups_created: u32,
    areas_created: u32,
    areas_unchanged: u32,
    /// Open areas of the same place that started earlier, ended the day
    /// before a new one.
    areas_ended: u32,
    settings_created: u32,
}

#[derive(Debug, Deserialize)]
struct ImportRow {
    country: String,
    code: String,
    rate: Decimal,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    postcode_from: Option<String>,
    #[serde(default)]
    postcode_to: Option<String>,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    valid_from: Option<NaiveDate>,
    #[serde(default)]
    valid_to: Option<NaiveDate>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    authority: Option<String>,
    #[serde(default)]
    registration: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

type AreaKey = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    NaiveDate,
);

fn blank(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

pub(crate) struct Import;

#[async_trait]
impl Command for Import {
    type Input = ImportInput;
    type Output = ImportOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "tax_area.import",
            "Import a rate table: which tax codes are charged where. One CSV row per \
             jurisdiction of an area (country, region, post code range, city, from a date); the \
             rows of one area, in order, become a group of their codes. A code is created where \
             it does not exist and refused where it exists at another rate (a rate change is a \
             new code). An open area of the same place that started earlier ends the day before \
             a new one. Optionally gives a company's accounts to codes it has none for. Takes \
             tax_area:import for all companies.",
        )
        .permission("tax_area:import")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ImportInput) -> Result<ImportOutput> {
        ctx.actor.require_global("tax_area:import")?;
        let accounts = match (
            input.company_id,
            input.collected_account_id,
            input.paid_account_id,
        ) {
            (Some(company), Some(collected), Some(paid)) => {
                ctx.require("tax_settings:create", company)?;
                Some((company, collected, paid))
            }
            (None, None, None) => None,
            _ => {
                return Err(Error::validation(
                    "give the company with both accounts, or none of them",
                ));
            }
        };
        let mut reader = csv::ReaderBuilder::new()
            .trim(csv::Trim::All)
            .from_reader(input.csv.as_bytes());
        let mut areas: Vec<(AreaKey, Vec<Uuid>, Option<String>, Option<NaiveDate>)> = vec![];
        let mut out = ImportOutput::default();
        let mut codes: BTreeMap<String, Uuid> = BTreeMap::new();
        for (index, row) in reader.deserialize::<ImportRow>().enumerate() {
            let line = index + 2;
            let bad =
                |why: String| Error::validation(format!("line {line}: {why}")).with_field("csv");
            let row = row.map_err(|e| bad(e.to_string()))?;
            let country = row.country.to_uppercase();
            if country.len() != 2 || !country.chars().all(|c| c.is_ascii_uppercase()) {
                return Err(bad(format!("{country} is not a two-letter country code")));
            }
            let region = blank(row.region).map(|r| r.to_uppercase());
            let postcode_from = blank(row.postcode_from).map(|p| normal_postcode(&p));
            let postcode_to = blank(row.postcode_to).map(|p| normal_postcode(&p));
            if postcode_from.is_some() != postcode_to.is_some()
                || postcode_from.as_ref().map(String::len) != postcode_to.as_ref().map(String::len)
                || postcode_from > postcode_to
            {
                return Err(bad(
                    "a post code range has a first and a last code of the same length, in order"
                        .into(),
                ));
            }
            let city = blank(row.city);
            let valid_from = row
                .valid_from
                .unwrap_or_else(|| NaiveDate::from_ymd_opt(2000, 1, 1).expect("a date"));
            if row.rate < Decimal::ZERO || row.rate > Decimal::ONE_HUNDRED {
                return Err(bad("a rate is from 0 to 100 percent".into()));
            }
            let jurisdiction_region = match row.registration.as_deref().map(str::trim) {
                None | Some("") | Some("region") => region.clone(),
                Some("country") => None,
                Some(other) => {
                    return Err(bad(format!(
                        "registration is region or country, not {other}"
                    )));
                }
            };
            let code = row.code.trim().to_owned();
            if code.is_empty() || code.contains('+') {
                return Err(bad("a code is given, without '+'".into()));
            }
            let id = match codes.get(&code) {
                Some(id) => *id,
                None => {
                    let existing: Option<(Uuid, Decimal, bool)> = sqlx::query_as(
                        "SELECT id, rate, EXISTS (SELECT 1 FROM tax_code_component g
                                                  WHERE g.group_id = tax_code.id)
                         FROM tax_code WHERE code = $1",
                    )
                    .bind(&code)
                    .fetch_optional(&mut *ctx.conn)
                    .await?;
                    let id = match existing {
                        Some((_, _, true)) => {
                            return Err(bad(format!("{code} is a group, not one jurisdiction")));
                        }
                        Some((id, rate, false)) if rate == row.rate => id,
                        Some((_, rate, false)) => {
                            return Err(bad(format!(
                                "{code} exists at {}; a rate change is a new code",
                                rate.normalize()
                            )));
                        }
                        None => {
                            let id = new_id();
                            let name = blank(row.name.clone())
                                .unwrap_or_else(|| format!("{code} {} %", row.rate.normalize()));
                            sqlx::query(
                                "INSERT INTO tax_code (id, code, name, rate, authority,
                                                       country_code, region)
                                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                            )
                            .bind(id)
                            .bind(&code)
                            .bind(&name)
                            .bind(row.rate)
                            .bind(blank(row.authority.clone()))
                            .bind(&country)
                            .bind(&jurisdiction_region)
                            .execute(&mut *ctx.conn)
                            .await?;
                            ctx.touch("tax_code", id);
                            out.codes_created += 1;
                            id
                        }
                    };
                    codes.insert(code.clone(), id);
                    id
                }
            };
            let key: AreaKey = (
                country,
                region,
                postcode_from,
                postcode_to,
                city,
                valid_from,
            );
            match areas.iter_mut().find(|(k, ..)| *k == key) {
                Some((_, parts, ..)) => {
                    if !parts.contains(&id) {
                        parts.push(id);
                    }
                }
                None => areas.push((key, vec![id], blank(row.source), row.valid_to)),
            }
        }
        for (key, parts, source, valid_to) in areas {
            let code = if parts.len() == 1 {
                parts[0]
            } else {
                let before: i64 = sqlx::query_scalar("SELECT count(*) FROM tax_code")
                    .fetch_one(&mut *ctx.conn)
                    .await?;
                let group = group_of(&mut *ctx.conn, &parts).await?;
                let after: i64 = sqlx::query_scalar("SELECT count(*) FROM tax_code")
                    .fetch_one(&mut *ctx.conn)
                    .await?;
                if after > before {
                    out.groups_created += 1;
                    ctx.touch("tax_code", group);
                }
                group
            };
            let (country, region, postcode_from, postcode_to, city, valid_from) = key;
            let existing: Option<(Uuid, Uuid)> = sqlx::query_as(
                "SELECT id, tax_code_id FROM tax_area
                 WHERE country_code = $1 AND region IS NOT DISTINCT FROM $2
                   AND postcode_from IS NOT DISTINCT FROM $3 AND postcode_to IS NOT DISTINCT FROM $4
                   AND city IS NOT DISTINCT FROM $5 AND valid_from = $6",
            )
            .bind(&country)
            .bind(&region)
            .bind(&postcode_from)
            .bind(&postcode_to)
            .bind(&city)
            .bind(valid_from)
            .fetch_optional(&mut *ctx.conn)
            .await?;
            match existing {
                Some((_, same)) if same == code => out.areas_unchanged += 1,
                Some((area, _)) => {
                    sqlx::query(
                        "UPDATE tax_area SET tax_code_id = $2, valid_to = $3, source = $4 WHERE id = $1",
                    )
                    .bind(area)
                    .bind(code)
                    .bind(valid_to)
                    .bind(&source)
                    .execute(&mut *ctx.conn)
                    .await?;
                    ctx.touch("tax_area", area);
                    out.areas_created += 1;
                }
                None => {
                    let ended = sqlx::query(
                        "UPDATE tax_area SET valid_to = $7 - 1
                         WHERE country_code = $1 AND region IS NOT DISTINCT FROM $2
                           AND postcode_from IS NOT DISTINCT FROM $3
                           AND postcode_to IS NOT DISTINCT FROM $4
                           AND city IS NOT DISTINCT FROM $5
                           AND valid_from < $6 AND (valid_to IS NULL OR valid_to >= $6)",
                    )
                    .bind(&country)
                    .bind(&region)
                    .bind(&postcode_from)
                    .bind(&postcode_to)
                    .bind(&city)
                    .bind(valid_from)
                    .bind(valid_from)
                    .execute(&mut *ctx.conn)
                    .await?
                    .rows_affected();
                    out.areas_ended += ended as u32;
                    let id = new_id();
                    sqlx::query(
                        "INSERT INTO tax_area (id, country_code, region, postcode_from, postcode_to,
                                               city, tax_code_id, valid_from, valid_to, source)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                    )
                    .bind(id)
                    .bind(&country)
                    .bind(&region)
                    .bind(&postcode_from)
                    .bind(&postcode_to)
                    .bind(&city)
                    .bind(code)
                    .bind(valid_from)
                    .bind(valid_to)
                    .bind(&source)
                    .execute(&mut *ctx.conn)
                    .await?;
                    ctx.touch("tax_area", id);
                    out.areas_created += 1;
                }
            }
        }
        if let Some((company, collected, paid)) = accounts {
            for id in codes.values() {
                let created = sqlx::query(
                    "INSERT INTO tax_settings (id, company_id, tax_code_id, collected_account_id,
                                               paid_account_id)
                     VALUES ($1, $2, $3, $4, $5)
                     ON CONFLICT (company_id, tax_code_id) DO NOTHING",
                )
                .bind(new_id())
                .bind(company)
                .bind(id)
                .bind(collected)
                .bind(paid)
                .execute(&mut *ctx.conn)
                .await?
                .rows_affected();
                out.settings_created += created as u32;
            }
        }
        Ok(out)
    }
}

// --------------------------------------------------------- OSS report

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct OssInput {
    /// The company registered for the One-Stop Shop.
    company_id: Uuid,
    /// The calendar year of the quarter.
    year: i32,
    /// 1 to 4.
    quarter: u32,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct OssRow {
    /// The member state of consumption.
    country_code: String,
    rate: Decimal,
    base: Decimal,
    vat: Decimal,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct OssCorrection {
    country_code: String,
    /// The period corrected.
    year: i32,
    quarter: i32,
    base: Decimal,
    vat: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct OssOutput {
    company_id: Uuid,
    /// The member state of identification.
    country_code: String,
    from: NaiveDate,
    to: NaiveDate,
    currency: String,
    /// Sales to consumers taxed in each member state of consumption, by
    /// rate, in the quarter.
    rows: Vec<OssRow>,
    /// Credits and reversals posted in the quarter of sales of earlier
    /// quarters.
    corrections: Vec<OssCorrection>,
    total_base: Decimal,
    total_vat: Decimal,
}

pub(crate) struct OssReport;

const OSS_ENTRIES: &str = "
    FROM tax_entry e
    JOIN tax_code c ON c.id = e.tax_code_id
    LEFT JOIN tax_entry o ON o.id = COALESCE(e.reverses_id, e.credits_id)
    WHERE e.company_id = $1 AND e.direction = 'collected' AND e.consumer
      AND NOT e.self_assessed AND e.posting_date BETWEEN $2 AND $3
      AND e.country_code <> $4 AND c.country_code = e.country_code
      AND e.country_code IN (SELECT b.country_code FROM tax_union_member a
                             JOIN tax_union_member b ON b.union_code = a.union_code
                             WHERE a.country_code = $4)";

#[async_trait]
impl Command for OssReport {
    type Input = OssInput;
    type Output = OssOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "tax.oss_report",
            "The EU One-Stop Shop return of a company for a quarter: per member state of \
             consumption and rate, the base and the VAT of sales to consumers taxed there, and \
             the corrections (credits and reversals posted in the quarter) of sales of earlier \
             quarters. Needs a One-Stop Shop registration (tax_registration, scheme oss).",
        )
        .report("OSS return")
        .permission("tax_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: OssInput) -> Result<OssOutput> {
        ctx.require("tax_entry:read", input.company_id)?;
        if !(1..=4).contains(&input.quarter) {
            return Err(Error::validation("a quarter is 1 to 4").with_field("quarter"));
        }
        let from = NaiveDate::from_ymd_opt(input.year, input.quarter * 3 - 2, 1)
            .ok_or_else(|| Error::validation("no such year").with_field("year"))?;
        let to = from
            .checked_add_months(chrono::Months::new(3))
            .and_then(|d| d.pred_opt())
            .ok_or_else(|| Error::validation("no such year").with_field("year"))?;
        let home: Option<String> = sqlx::query_scalar(
            "SELECT country_code FROM tax_registration
             WHERE company_id = $1 AND scheme = 'oss' AND valid_from <= $3
               AND (valid_to IS NULL OR valid_to >= $2)
             ORDER BY valid_from DESC LIMIT 1",
        )
        .bind(input.company_id)
        .bind(from)
        .bind(to)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        let home = home.ok_or_else(|| {
            Error::validation("the company has no One-Stop Shop registration in the quarter")
        })?;
        let currency = company::base_currency(&mut *ctx.conn, input.company_id).await?;
        let rows: Vec<OssRow> = sqlx::query_as(&format!(
            "SELECT e.country_code, e.rate, sum(e.base) AS base, sum(e.amount) AS vat
             {OSS_ENTRIES} AND (o.id IS NULL OR o.posting_date >= $2)
             GROUP BY e.country_code, e.rate ORDER BY e.country_code, e.rate"
        ))
        .bind(input.company_id)
        .bind(from)
        .bind(to)
        .bind(&home)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let corrections: Vec<OssCorrection> = sqlx::query_as(&format!(
            "SELECT e.country_code, extract(year FROM o.posting_date)::int AS year,
                    extract(quarter FROM o.posting_date)::int AS quarter,
                    sum(e.base) AS base, sum(e.amount) AS vat
             {OSS_ENTRIES} AND o.posting_date < $2
             GROUP BY 1, 2, 3 ORDER BY 1, 2, 3"
        ))
        .bind(input.company_id)
        .bind(from)
        .bind(to)
        .bind(&home)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let total_base = rows.iter().map(|r| r.base).sum::<Decimal>()
            + corrections.iter().map(|r| r.base).sum::<Decimal>();
        let total_vat = rows.iter().map(|r| r.vat).sum::<Decimal>()
            + corrections.iter().map(|r| r.vat).sum::<Decimal>();
        Ok(OssOutput {
            company_id: input.company_id,
            country_code: home,
            from,
            to,
            currency,
            rows,
            corrections,
            total_base,
            total_vat,
        })
    }
}

// ------------------------------------------------------- exemptions

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExemptionsInput {
    /// The company to report on.
    company_id: Uuid,
    /// First posting date to include.
    from: NaiveDate,
    /// Last posting date to include.
    to: NaiveDate,
}

#[derive(Serialize, JsonSchema, sqlx::FromRow)]
pub(crate) struct ExemptionRow {
    exemption_id: Uuid,
    customer_id: Uuid,
    customer_code: String,
    certificate_number: String,
    country_code: String,
    region: Option<String>,
    reason: String,
    /// Net sales exempted under it in the range (credits below zero).
    base: Decimal,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct ExemptionsOutput {
    company_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    rows: Vec<ExemptionRow>,
    total_base: Decimal,
}

pub(crate) struct ExemptionsReport;

#[async_trait]
impl Command for ExemptionsReport {
    type Input = ExemptionsInput;
    type Output = ExemptionsOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "tax.exemptions",
            "Sales exempted under customers' exemption certificates over a range of posting \
             dates: per certificate, the customer, its number, where it holds, the reason and \
             the net amount exempted.",
        )
        .report("Exempt sales")
        .permission("tax_entry:read")
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ExemptionsInput) -> Result<ExemptionsOutput> {
        ctx.require("tax_entry:read", input.company_id)?;
        if input.from > input.to {
            return Err(Error::validation("'from' must not be after 'to'"));
        }
        let rows: Vec<ExemptionRow> = sqlx::query_as(
            "SELECT x.id AS exemption_id, x.customer_id, cu.code AS customer_code,
                    x.certificate_number, x.country_code, x.region, x.reason,
                    sum(e.base) AS base
             FROM tax_entry e
             JOIN tax_exemption x ON x.id = e.exemption_id
             JOIN customer cu ON cu.id = x.customer_id
             WHERE e.company_id = $1 AND e.posting_date BETWEEN $2 AND $3
             GROUP BY x.id, cu.code ORDER BY cu.code, x.certificate_number",
        )
        .bind(input.company_id)
        .bind(input.from)
        .bind(input.to)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let total_base = rows.iter().map(|r| r.base).sum();
        Ok(ExemptionsOutput {
            company_id: input.company_id,
            from: input.from,
            to: input.to,
            rows,
            total_base,
        })
    }
}
