//! The data a tax auditor is given (Datenträgerüberlassung, Z3 access):
//! the tables of a fiscal year with an `index.xml` that describes them by
//! the Beschreibungsstandard of the GDPdU (the DTD
//! `gdpdu-01-08-2002.dtd`), which IDEA and the other audit tools read.

use chrono::{NaiveDate, Utc};
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Decimal, Error, JsonSchema, Result, async_trait, schemars,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::file::ExportFile;

const DTD: &[u8] = include_bytes!("../assets/gdpdu-01-08-2002.dtd");
const DTD_NAME: &str = "gdpdu-01-08-2002.dtd";
const PERMISSION: &str = "gdpdu:export";

/// How a column is typed in the description.
#[derive(Clone, Copy)]
enum Kind {
    Text,
    /// With this many decimal places.
    Number(u32),
    Date,
}

struct Column {
    name: &'static str,
    description: &'static str,
    kind: Kind,
}

const fn col(name: &'static str, description: &'static str, kind: Kind) -> Column {
    Column {
        name,
        description,
        kind,
    }
}

/// A table: its file, name, description, primary key (the first `keys`
/// columns), columns and foreign keys (column, table it references).
struct Table {
    file: &'static str,
    name: &'static str,
    description: &'static str,
    keys: usize,
    columns: &'static [Column],
    foreign: &'static [(&'static str, &'static str)],
}

use Kind::{Date, Number, Text};

const ACCOUNTS: Table = Table {
    file: "konten.csv",
    name: "Konten",
    description: "Sachkonten mit Anfangsbestand, Verkehrszahlen und Endbestand des Zeitraums",
    keys: 1,
    columns: &[
        col("Kontonummer", "Nummer des Sachkontos", Text),
        col("Kontobezeichnung", "Bezeichnung des Kontos", Text),
        col(
            "Kontoart",
            "asset, liability, equity, income oder expense",
            Text,
        ),
        col(
            "Anfangsbestand",
            "Saldo Soll minus Haben vor Beginn des Zeitraums",
            Number(2),
        ),
        col("Soll", "Summe der Sollbuchungen im Zeitraum", Number(2)),
        col("Haben", "Summe der Habenbuchungen im Zeitraum", Number(2)),
        col(
            "Endbestand",
            "Saldo Soll minus Haben am Ende des Zeitraums",
            Number(2),
        ),
    ],
    foreign: &[],
};

const DOCUMENTS: Table = Table {
    file: "belege.csv",
    name: "Belege",
    description: "Belege, die im Zeitraum datiert oder gebucht sind, ohne Entwürfe",
    keys: 1,
    columns: &[
        col("BelegID", "Eindeutige Kennung des Belegs", Text),
        col("Belegart", "Art des Belegs, z. B. sales_invoice", Text),
        col("Belegnummer", "Nummer aus dem Nummernkreis", Text),
        col(
            "Status",
            "Status des Belegs, z. B. posted oder reversed",
            Text,
        ),
        col("Belegdatum", "Datum des Belegs", Date),
        col("ErfasstAm", "Tag der Erfassung (UTC)", Date),
        col("ErfasstVon", "Benutzer, der den Beleg erfasst hat", Text),
        col("Partnerart", "customer oder vendor", Text),
        col("Partnernummer", "Nummer des Kunden oder Lieferanten", Text),
        col("Netto", "Nettobetrag in Belegwährung", Number(2)),
        col("Steuer", "Steuerbetrag in Belegwährung", Number(2)),
        col("Brutto", "Bruttobetrag in Belegwährung", Number(2)),
        col("Waehrung", "Währung des Belegs", Text),
        col("Bemerkung", "Bemerkung zum Beleg", Text),
    ],
    foreign: &[],
};

const JOURNAL: Table = Table {
    file: "buchungen.csv",
    name: "Buchungen",
    description: "Alle Buchungszeilen des Hauptbuchs im Zeitraum (Journal)",
    keys: 2,
    columns: &[
        col("BuchungID", "Eindeutige Kennung der Buchung", Text),
        col("Zeile", "Zeile der Buchung", Number(0)),
        col("Buchungsdatum", "Datum der Buchung", Date),
        col("ErfasstAm", "Tag der Erfassung (UTC)", Date),
        col("ErfasstVon", "Benutzer, der gebucht hat", Text),
        col("BelegID", "Der Beleg der Buchung", Text),
        col("Belegart", "Art des Belegs", Text),
        col("Belegnummer", "Nummer des Belegs", Text),
        col("Kontonummer", "Sachkonto", Text),
        col("Soll", "Soll in Hauswährung", Number(2)),
        col("Haben", "Haben in Hauswährung", Number(2)),
        col("Waehrung", "Währung der Buchungszeile", Text),
        col("SollFW", "Soll in der Währung der Zeile", Number(2)),
        col("HabenFW", "Haben in der Währung der Zeile", Number(2)),
        col(
            "Kurs",
            "Hauswährung je Einheit der Währung der Zeile",
            Number(6),
        ),
        col("Buchungstext", "Text der Buchungszeile", Text),
        col(
            "Partnerart",
            "customer oder vendor, wenn die Zeile einen Partner betrifft",
            Text,
        ),
        col("Partnernummer", "Nummer des Kunden oder Lieferanten", Text),
        col("StorniertBuchung", "Die Buchung, die diese storniert", Text),
        col(
            "Abschluss",
            "1 für eine Abschlussbuchung, sonst 0",
            Number(0),
        ),
        col(
            "Dimensionen",
            "Kostenstellen und andere Dimensionen als DIMENSION:WERT",
            Text,
        ),
    ],
    foreign: &[("Kontonummer", "Konten"), ("BelegID", "Belege")],
};

const TAX: Table = Table {
    file: "steuer.csv",
    name: "Steuer",
    description: "Steuerzeilen der Buchungen im Zeitraum (Umsatzsteuer und Vorsteuer)",
    keys: 2,
    columns: &[
        col("BuchungID", "Die Buchung", Text),
        col("Zeile", "Zeile der Steuer in der Buchung", Number(0)),
        col("Buchungsdatum", "Datum der Buchung", Date),
        col("BelegID", "Der Beleg der Buchung", Text),
        col("Steuerschluessel", "Steuerschlüssel", Text),
        col("Steuersatz", "Steuersatz in Prozent", Number(4)),
        col(
            "Richtung",
            "collected (Umsatzsteuer) oder paid (Vorsteuer)",
            Text,
        ),
        col("Bemessungsgrundlage", "Netto in Hauswährung", Number(2)),
        col("Steuerbetrag", "Steuer in Hauswährung", Number(2)),
        col("Steuerkonto", "Das Konto der Steuer", Text),
        col("Land", "Land des Leistungsorts", Text),
        col(
            "Steuerschuldnerschaft",
            "1 bei Steuerschuldnerschaft des Leistungsempfängers",
            Number(0),
        ),
    ],
    foreign: &[("BelegID", "Belege")],
};

const PARTIES: Table = Table {
    file: "partner.csv",
    name: "Partner",
    description: "Kunden und Lieferanten mit offenen Posten des Mandanten",
    keys: 2,
    columns: &[
        col("Partnerart", "customer oder vendor", Text),
        col("Partnernummer", "Nummer des Kunden oder Lieferanten", Text),
        col("Name", "Name", Text),
        col("UStIdNr", "Umsatzsteuer-Identifikationsnummer", Text),
        col("Steuernummer", "Steuernummer", Text),
        col("Strasse", "Straße", Text),
        col("PLZ", "Postleitzahl", Text),
        col("Ort", "Ort", Text),
        col("Land", "Land", Text),
    ],
    foreign: &[],
};

#[derive(sqlx::FromRow)]
struct JournalRow {
    posting_id: Uuid,
    line_no: i32,
    posting_date: NaiveDate,
    created_at: chrono::DateTime<Utc>,
    user_name: Option<String>,
    document_id: Uuid,
    doc_type: String,
    number: Option<String>,
    code: String,
    debit: Decimal,
    credit: Decimal,
    currency: String,
    txn_debit: Decimal,
    txn_credit: Decimal,
    fx_rate: Decimal,
    memo: Option<String>,
    party_type: Option<String>,
    party_code: Option<String>,
    reversed_by: Option<Uuid>,
    closing: bool,
    dimensions: Option<String>,
}

/// A field of a data file: text in double quotes (a quote doubled),
/// numbers with a decimal comma, dates as DD.MM.YYYY, nothing for empty.
enum Field {
    Text(Option<String>),
    Number(Option<Decimal>, u32),
    Date(Option<NaiveDate>),
}

fn field(f: &Field) -> String {
    match f {
        Field::Text(None) | Field::Number(None, _) | Field::Date(None) => String::new(),
        Field::Text(Some(t)) => {
            let t: String = t
                .chars()
                .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
                .collect();
            format!("\"{}\"", t.replace('"', "\"\""))
        }
        Field::Number(Some(n), places) => {
            format!("{:.*}", *places as usize, n.round_dp(*places)).replace('.', ",")
        }
        Field::Date(Some(d)) => d.format("%d.%m.%Y").to_string(),
    }
}

fn file(rows: Vec<Vec<Field>>) -> Vec<u8> {
    let mut out = String::new();
    for row in rows {
        out.push_str(&row.iter().map(field).collect::<Vec<_>>().join(";"));
        out.push_str("\r\n");
    }
    out.into_bytes()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The description of the tables, `index.xml`.
fn index(
    supplier: &str,
    location: &str,
    from: NaiveDate,
    to: NaiveDate,
    tables: &[&Table],
) -> String {
    let date = |d: NaiveDate| d.format("%d.%m.%Y").to_string();
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n");
    x.push_str(&format!("<!DOCTYPE DataSet SYSTEM \"{DTD_NAME}\">\r\n"));
    x.push_str("<DataSet>\r\n  <Version>1.0</Version>\r\n");
    x.push_str(&format!(
        "  <DataSupplier>\r\n    <Name>{}</Name>\r\n    <Location>{}</Location>\r\n    \
         <Comment>erpcraft {}, Zeitraum {} bis {}</Comment>\r\n  </DataSupplier>\r\n",
        escape(supplier),
        escape(location),
        env!("CARGO_PKG_VERSION"),
        date(from),
        date(to)
    ));
    x.push_str("  <Media>\r\n    <Name>Datenträger 1</Name>\r\n");
    for t in tables {
        x.push_str("    <Table>\r\n");
        x.push_str(&format!("      <URL>{}</URL>\r\n", t.file));
        x.push_str(&format!("      <Name>{}</Name>\r\n", t.name));
        x.push_str(&format!(
            "      <Description>{}</Description>\r\n",
            escape(t.description)
        ));
        x.push_str(&format!(
            "      <Validity>\r\n        <Range>\r\n          <From>{}</From>\r\n          \
             <To>{}</To>\r\n        </Range>\r\n        <Format>DD.MM.YYYY</Format>\r\n      \
             </Validity>\r\n",
            date(from),
            date(to)
        ));
        x.push_str("      <UTF8/>\r\n");
        x.push_str("      <DecimalSymbol>,</DecimalSymbol>\r\n");
        x.push_str("      <DigitGroupingSymbol>.</DigitGroupingSymbol>\r\n");
        x.push_str("      <VariableLength>\r\n");
        x.push_str("        <ColumnDelimiter>;</ColumnDelimiter>\r\n");
        x.push_str("        <RecordDelimiter>&#13;&#10;</RecordDelimiter>\r\n");
        x.push_str("        <TextEncapsulator>\"</TextEncapsulator>\r\n");
        for (i, c) in t.columns.iter().enumerate() {
            let element = if i < t.keys {
                "VariablePrimaryKey"
            } else {
                "VariableColumn"
            };
            let kind = match c.kind {
                Kind::Text => "<AlphaNumeric/>".to_owned(),
                Kind::Number(0) => "<Numeric/>".to_owned(),
                Kind::Number(n) => format!("<Numeric><Accuracy>{n}</Accuracy></Numeric>"),
                Kind::Date => "<Date><Format>DD.MM.YYYY</Format></Date>".to_owned(),
            };
            x.push_str(&format!(
                "        <{element}>\r\n          <Name>{}</Name>\r\n          \
                 <Description>{}</Description>\r\n          {kind}\r\n        </{element}>\r\n",
                c.name,
                escape(c.description)
            ));
        }
        for (column, references) in t.foreign {
            x.push_str(&format!(
                "        <ForeignKey>\r\n          <Name>{column}</Name>\r\n          \
                 <References>{references}</References>\r\n        </ForeignKey>\r\n"
            ));
        }
        x.push_str("      </VariableLength>\r\n    </Table>\r\n");
    }
    x.push_str("  </Media>\r\n</DataSet>\r\n");
    x
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportInput {
    /// The company audited.
    company_id: Uuid,
    /// The first day of the fiscal year.
    from: NaiveDate,
    /// Its last day: at most a year after `from`.
    to: NaiveDate,
}

pub(crate) struct Export;

#[async_trait]
impl Command for Export {
    type Input = ExportInput;
    type Output = ExportFile;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "gdpdu.export",
            "The data for a German tax audit (Datenträgerüberlassung, Z3) of a fiscal year, as a \
             ZIP archive: index.xml described by the GDPdU Beschreibungsstandard with its DTD, \
             and the tables Konten, Belege, Buchungen (every general ledger entry), Steuer and \
             Partner, in UTF-8 with a decimal comma. The whole of the company's books for the \
             range, whatever rules limit the reader otherwise: it takes gdpdu:export. Recorded \
             in export_log.",
        )
        .permission(PERMISSION)
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: ExportInput) -> Result<ExportFile> {
        let company_id = input.company_id;
        ctx.require(PERMISSION, company_id)?;
        let (from, to) = (input.from, input.to);
        if from > to {
            return Err(Error::validation("`from` cannot be after `to`").with_field("from"));
        }
        if (to - from).num_days() > 366 {
            return Err(
                Error::validation("an audit export covers one fiscal year at most")
                    .with_field("to"),
            );
        }
        let (name, code): (String, String) =
            sqlx::query_as("SELECT name, code FROM company WHERE id = $1")
                .bind(company_id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        let location: Option<String> = sqlx::query_scalar(
            "SELECT concat_ws(', ', street, concat_ws(' ', postcode, city), country_code)
             FROM party_address WHERE party_type = 'company' AND party_id = $1
             ORDER BY is_default DESC LIMIT 1",
        )
        .bind(company_id)
        .fetch_optional(&mut *ctx.conn)
        .await?;

        let text = |v: Option<String>| Field::Text(v);
        let money = |v: Option<Decimal>| Field::Number(v, 2);

        // Konten
        let accounts: Vec<(String, String, String, Decimal, Decimal, Decimal)> = sqlx::query_as(
            "SELECT a.code, a.name, a.account_type,
                    COALESCE(sum(g.debit - g.credit) FILTER (WHERE g.posting_date < $2), 0),
                    COALESCE(sum(g.debit) FILTER (WHERE g.posting_date BETWEEN $2 AND $3), 0),
                    COALESCE(sum(g.credit) FILTER (WHERE g.posting_date BETWEEN $2 AND $3), 0)
             FROM account a LEFT JOIN gl_entry g ON g.account_id = a.id AND g.posting_date <= $3
             WHERE a.company_id = $1 AND NOT a.is_group
             GROUP BY a.id ORDER BY a.code",
        )
        .bind(company_id)
        .bind(from)
        .bind(to)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let konten: Vec<Vec<Field>> = accounts
            .into_iter()
            .map(|(code, name, kind, opening, debit, credit)| {
                vec![
                    text(Some(code)),
                    text(Some(name)),
                    text(Some(kind)),
                    money(Some(opening)),
                    money(Some(debit)),
                    money(Some(credit)),
                    money(Some(opening + debit - credit)),
                ]
            })
            .collect();

        // Belege
        #[allow(clippy::type_complexity)]
        let docs: Vec<(
            Uuid,
            String,
            Option<String>,
            String,
            NaiveDate,
            chrono::DateTime<Utc>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT d.id, d.doc_type, d.number, d.status, d.doc_date, d.created_at, u.name,
                    CASE WHEN d.summary ? 'customer_id' THEN 'customer'
                         WHEN d.summary ? 'vendor_id' THEN 'vendor' END,
                    COALESCE(c.code, v.code),
                    d.summary->>'total_net', d.summary->>'total_tax', d.summary->>'total_gross',
                    d.summary->>'currency', d.memo
             FROM document d
             LEFT JOIN app_user u ON u.id = d.created_by
             LEFT JOIN customer c ON c.id::text = d.summary->>'customer_id'
             LEFT JOIN vendor v ON v.id::text = d.summary->>'vendor_id'
             WHERE d.company_id = $1 AND d.status <> 'draft'
               AND (d.doc_date BETWEEN $2 AND $3
                    OR EXISTS (SELECT 1 FROM posting p WHERE p.document_id = d.id
                               AND p.posting_date BETWEEN $2 AND $3))
             ORDER BY d.doc_date, d.number, d.id",
        )
        .bind(company_id)
        .bind(from)
        .bind(to)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let number = |v: Option<String>| v.and_then(|s| s.parse::<Decimal>().ok());
        let belege: Vec<Vec<Field>> = docs
            .into_iter()
            .map(|d| {
                vec![
                    text(Some(d.0.to_string())),
                    text(Some(d.1)),
                    text(d.2),
                    text(Some(d.3)),
                    Field::Date(Some(d.4)),
                    Field::Date(Some(d.5.date_naive())),
                    text(d.6),
                    text(d.7),
                    text(d.8),
                    money(number(d.9)),
                    money(number(d.10)),
                    money(number(d.11)),
                    text(d.12),
                    text(d.13),
                ]
            })
            .collect();

        // Buchungen
        let entries: Vec<JournalRow> = sqlx::query_as(
            "SELECT g.posting_id, g.line_no, g.posting_date, p.created_at, u.name AS user_name,
                    d.id AS document_id, d.doc_type, d.number, a.code, g.debit, g.credit,
                    g.currency, g.txn_debit, g.txn_credit, g.fx_rate,
                    COALESCE(g.memo, p.memo) AS memo, pt.party_type,
                    COALESCE(c.code, v.code) AS party_code, rev.id AS reversed_by,
                    starts_with(p.command, 'year_end_close.') AS closing,
                    (SELECT string_agg(dm.code || ':' || dv.code, ', ' ORDER BY dm.code)
                     FROM dimension_set_value sv JOIN dimension dm ON dm.id = sv.dimension_id
                     JOIN dimension_value dv ON dv.id = sv.value_id
                     WHERE sv.set_id = g.dimension_set_id) AS dimensions
             FROM gl_entry g JOIN posting p ON p.id = g.posting_id
             JOIN document d ON d.id = p.document_id
             JOIN account a ON a.id = g.account_id
             LEFT JOIN app_user u ON u.id = p.created_by
             LEFT JOIN posting rev ON rev.reverses_id = p.id
             LEFT JOIN LATERAL (
                 SELECT o.party_type, o.party_id FROM open_item o
                 WHERE o.account_id = g.account_id
                   AND (o.posting_id = COALESCE(p.reverses_id, p.id)
                        OR o.id IN (SELECT st.open_item_id FROM settlement st
                                    WHERE st.posting_id = p.id))
                 ORDER BY o.id LIMIT 1) pt ON true
             LEFT JOIN customer c ON pt.party_type = 'customer' AND c.id = pt.party_id
             LEFT JOIN vendor v ON pt.party_type = 'vendor' AND v.id = pt.party_id
             WHERE g.company_id = $1 AND g.posting_date BETWEEN $2 AND $3
             ORDER BY g.posting_date, p.created_at, g.posting_id, g.line_no",
        )
        .bind(company_id)
        .bind(from)
        .bind(to)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let buchungen: Vec<Vec<Field>> = entries
            .into_iter()
            .map(|e| {
                vec![
                    text(Some(e.posting_id.to_string())),
                    Field::Number(Some(Decimal::from(e.line_no)), 0),
                    Field::Date(Some(e.posting_date)),
                    Field::Date(Some(e.created_at.date_naive())),
                    text(e.user_name),
                    text(Some(e.document_id.to_string())),
                    text(Some(e.doc_type)),
                    text(e.number),
                    text(Some(e.code)),
                    money(Some(e.debit)),
                    money(Some(e.credit)),
                    text(Some(e.currency)),
                    money(Some(e.txn_debit)),
                    money(Some(e.txn_credit)),
                    Field::Number(Some(e.fx_rate), 6),
                    text(e.memo),
                    text(e.party_type),
                    text(e.party_code),
                    text(e.reversed_by.map(|r| r.to_string())),
                    Field::Number(Some(Decimal::from(i32::from(e.closing))), 0),
                    text(e.dimensions),
                ]
            })
            .collect();

        // Steuer
        #[allow(clippy::type_complexity)]
        let taxes: Vec<(
            Uuid,
            i32,
            NaiveDate,
            Uuid,
            String,
            Decimal,
            String,
            Decimal,
            Decimal,
            Option<String>,
            Option<String>,
            bool,
        )> = sqlx::query_as(
            "SELECT t.posting_id, t.line_no, t.posting_date, p.document_id, c.code, t.rate,
                    t.direction, t.base, t.amount, a.code, t.country_code, t.self_assessed
             FROM tax_entry t JOIN posting p ON p.id = t.posting_id
             JOIN tax_code c ON c.id = t.tax_code_id
             LEFT JOIN account a ON a.id = t.account_id
             WHERE t.company_id = $1 AND t.posting_date BETWEEN $2 AND $3
             ORDER BY t.posting_date, t.posting_id, t.line_no",
        )
        .bind(company_id)
        .bind(from)
        .bind(to)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let steuer: Vec<Vec<Field>> = taxes
            .into_iter()
            .map(|t| {
                vec![
                    text(Some(t.0.to_string())),
                    Field::Number(Some(Decimal::from(t.1)), 0),
                    Field::Date(Some(t.2)),
                    text(Some(t.3.to_string())),
                    text(Some(t.4)),
                    Field::Number(Some(t.5), 4),
                    text(Some(t.6)),
                    money(Some(t.7)),
                    money(Some(t.8)),
                    text(t.9),
                    text(t.10),
                    Field::Number(Some(Decimal::from(i32::from(t.11))), 0),
                ]
            })
            .collect();

        // Partner
        #[allow(clippy::type_complexity)]
        let parties: Vec<(
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "WITH p AS (SELECT DISTINCT party_type, party_id FROM open_item
                        WHERE company_id = $1 AND party_type IN ('customer', 'vendor'))
             SELECT p.party_type, COALESCE(c.code, v.code), COALESCE(c.name, v.name),
                    (SELECT value FROM party_tax_id t WHERE t.party_type = p.party_type
                     AND t.party_id = p.party_id AND t.scheme = 'eu_vat'),
                    (SELECT value FROM party_tax_id t WHERE t.party_type = p.party_type
                     AND t.party_id = p.party_id AND t.scheme = 'tax_number'),
                    ad.street, ad.postcode, ad.city, ad.country_code
             FROM p
             LEFT JOIN customer c ON p.party_type = 'customer' AND c.id = p.party_id
             LEFT JOIN vendor v ON p.party_type = 'vendor' AND v.id = p.party_id
             LEFT JOIN LATERAL (
                 SELECT street, postcode, city, country_code FROM party_address a
                 WHERE a.party_type = p.party_type AND a.party_id = p.party_id
                 ORDER BY a.is_default DESC LIMIT 1) ad ON true
             ORDER BY 1, 2",
        )
        .bind(company_id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        let partner: Vec<Vec<Field>> = parties
            .into_iter()
            .map(|p| {
                vec![
                    text(Some(p.0)),
                    text(Some(p.1)),
                    text(Some(p.2)),
                    text(p.3),
                    text(p.4),
                    text(p.5),
                    text(p.6),
                    text(p.7),
                    text(p.8),
                ]
            })
            .collect();

        let rows = konten.len() + belege.len() + buchungen.len() + steuer.len() + partner.len();
        let tables = [&ACCOUNTS, &DOCUMENTS, &JOURNAL, &TAX, &PARTIES];
        let index = index(
            &name,
            location.as_deref().unwrap_or(&code),
            from,
            to,
            &tables,
        );
        let data = [
            file(konten),
            file(belege),
            file(buchungen),
            file(steuer),
            file(partner),
        ];
        let mut files: Vec<(&str, &[u8])> = vec![("index.xml", index.as_bytes()), (DTD_NAME, DTD)];
        for (t, bytes) in tables.iter().zip(&data) {
            files.push((t.file, bytes));
        }
        let zip = crate::zip::archive(&files, Utc::now().naive_utc())?;
        let made = ExportFile::new(
            format!(
                "gdpdu_{}_{}_{}.zip",
                code,
                from.format("%Y%m%d"),
                to.format("%Y%m%d")
            ),
            "application/zip",
            index,
            zip,
            rows,
            vec![],
        );
        made.log(ctx, company_id, "gdpdu", Some((from, to)), false)
            .await?;
        Ok(made)
    }
}
