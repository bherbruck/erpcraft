//! A trading partner as the module works with it: how to route to it, how
//! to address and separate what is written to it.

use chrono::NaiveDateTime;
use erpcraft_kernel::{Error, PgConnection, Result};
use uuid::Uuid;

use crate::segment::Delimiters;
use crate::x12::{Envelope, Isa};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Partner {
    pub id: Uuid,
    pub company_id: Uuid,
    pub code: String,
    pub name: String,
    pub customer_id: Uuid,
    pub warehouse_id: Option<Uuid>,
    pub syntax: String,
    pub usage: String,
    pub our_qualifier: String,
    pub our_id: String,
    pub their_qualifier: String,
    pub their_id: String,
    pub our_group_id: Option<String>,
    pub their_group_id: Option<String>,
    pub version: String,
    pub ack_form: String,
    pub ack_hours: i32,
    pub element_separator: String,
    pub component_separator: String,
    pub repetition_separator: Option<String>,
    pub segment_terminator: String,
    pub line_breaks: bool,
    pub run_as_user_id: Option<Uuid>,
    pub active: bool,
    /// EDIFACT: `.` or `,`.
    pub decimal_mark: String,
    /// EDIFACT: the character set, such as `UNOC`.
    pub charset: String,
    /// The partner's implementation guide, if it has one of its own.
    pub guide_id: Option<Uuid>,
}

const COLUMNS: &str = "id, company_id, code, name, customer_id, warehouse_id, syntax, usage, \
    our_qualifier, our_id, their_qualifier, their_id, our_group_id, their_group_id, version, \
    ack_form, ack_hours, element_separator, component_separator, repetition_separator, \
    segment_terminator, line_breaks, run_as_user_id, active, decimal_mark, charset, guide_id";

pub async fn load(conn: &mut PgConnection, id: Uuid) -> Result<Partner> {
    sqlx::query_as(&format!("SELECT {COLUMNS} FROM edi_partner WHERE id = $1"))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| Error::not_found("edi_partner"))
}

/// The partner an EDIFACT interchange is from: as for X12, by the UNB's
/// sender and recipient with their qualifiers, a partner of the
/// interchange's usage preferred.
pub async fn route_edifact(
    conn: &mut PgConnection,
    unb: &crate::edifact::Interchange,
    company_id: Option<Uuid>,
) -> Result<std::result::Result<Partner, String>> {
    let usage = if unb.test { "test" } else { "production" };
    let found: Vec<Partner> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM edi_partner
         WHERE active AND syntax = 'edifact' AND their_qualifier = $1 AND their_id = $2
           AND our_qualifier = $3 AND our_id = $4 AND ($5::uuid IS NULL OR company_id = $5)
         ORDER BY (usage = $6) DESC, created_at"
    ))
    .bind(&unb.sender_qualifier)
    .bind(&unb.sender)
    .bind(&unb.receiver_qualifier)
    .bind(&unb.receiver)
    .bind(company_id)
    .bind(usage)
    .fetch_all(&mut *conn)
    .await?;
    let route = format!(
        "{}:{} to {}:{}",
        unb.sender, unb.sender_qualifier, unb.receiver, unb.receiver_qualifier
    );
    let mut companies: Vec<Uuid> = found.iter().map(|p| p.company_id).collect();
    companies.sort();
    companies.dedup();
    if companies.len() > 1 {
        return Ok(Err(format!(
            "partners of more than one company are from {route}: receive it with the \
             company_id it is for"
        )));
    }
    Ok(found
        .into_iter()
        .next()
        .ok_or_else(|| format!("no active partner is from {route}")))
}

/// The partner an X12 interchange is from: the active partner whose
/// identifiers are the interchange's sender and receiver. A partner of the
/// interchange's usage is preferred (a test interchange to the test
/// partner), else the other. With `company_id`, only that company's.
/// Partners of more than one company with those identifiers are no
/// answer: the interchange must say which company it is for. The error is
/// the sentence kept on an unroutable interchange.
pub async fn route(
    conn: &mut PgConnection,
    isa: &Isa,
    company_id: Option<Uuid>,
) -> Result<std::result::Result<Partner, String>> {
    let usage = if isa.usage == 'P' {
        "production"
    } else {
        "test"
    };
    let found: Vec<Partner> = sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM edi_partner
         WHERE active AND syntax = 'x12' AND their_qualifier = $1 AND their_id = $2
           AND our_qualifier = $3 AND our_id = $4 AND ($5::uuid IS NULL OR company_id = $5)
         ORDER BY (usage = $6) DESC, created_at"
    ))
    .bind(&isa.sender_qualifier)
    .bind(&isa.sender_id)
    .bind(&isa.receiver_qualifier)
    .bind(&isa.receiver_id)
    .bind(company_id)
    .bind(usage)
    .fetch_all(&mut *conn)
    .await?;
    let route = format!(
        "{}/{} to {}/{}",
        isa.sender_qualifier, isa.sender_id, isa.receiver_qualifier, isa.receiver_id
    );
    let mut companies: Vec<Uuid> = found.iter().map(|p| p.company_id).collect();
    companies.sort();
    companies.dedup();
    if companies.len() > 1 {
        return Ok(Err(format!(
            "partners of more than one company are from {route}: receive it with the \
             company_id it is for"
        )));
    }
    Ok(found
        .into_iter()
        .next()
        .ok_or_else(|| format!("no active partner is from {route}")))
}

fn byte(text: &str, what: &str) -> Result<u8> {
    match text.as_bytes() {
        [b] => Ok(*b),
        _ => Err(Error::validation(format!(
            "the partner's {what} is not one character"
        ))),
    }
}

impl Partner {
    /// The separators to write with. A repetition separator is written
    /// only from version 005010, whose ISA can state one.
    pub fn delimiters(&self) -> Result<Delimiters> {
        let d = Delimiters {
            element: byte(&self.element_separator, "element separator")?,
            component: byte(&self.component_separator, "component separator")?,
            repetition: match (&self.repetition_separator, self.version.as_str()) {
                (Some(r), "005010") => Some(byte(r, "repetition separator")?),
                _ => None,
            },
            terminator: byte(&self.segment_terminator, "segment terminator")?,
            release: None,
            decimal: b'.',
        };
        d.check(0)
            .map_err(|e| Error::validation(format!("the partner's separators: {}", e.message)))?;
        Ok(d)
    }

    /// ISA12 for the partner's version.
    pub fn isa_version(&self) -> &'static str {
        match self.version.as_str() {
            "005010" => "00501",
            _ => "00401",
        }
    }

    /// Whether what is exchanged with this partner is test traffic.
    pub fn is_test(&self) -> bool {
        self.usage != "production"
    }

    /// The envelope of an interchange to this partner.
    pub fn envelope(
        &self,
        functional_id: &str,
        at: NaiveDateTime,
        interchange_control: u32,
        group_control: u32,
        usage: char,
    ) -> Result<Envelope> {
        Ok(Envelope {
            sender_qualifier: self.our_qualifier.clone(),
            sender_id: self.our_id.clone(),
            receiver_qualifier: self.their_qualifier.clone(),
            receiver_id: self.their_id.clone(),
            group_sender: self
                .our_group_id
                .clone()
                .unwrap_or_else(|| self.our_id.clone()),
            group_receiver: self
                .their_group_id
                .clone()
                .unwrap_or_else(|| self.their_id.clone()),
            at,
            version: self.isa_version().to_owned(),
            group_version: self.version.clone(),
            functional_id: functional_id.to_owned(),
            interchange_control,
            group_control,
            usage,
            ack_requested: false,
            delimiters: self.delimiters()?,
        })
    }

    /// Whether the partner trades in EDIFACT.
    pub fn is_edifact(&self) -> bool {
        self.syntax == "edifact"
    }

    /// The envelope of an EDIFACT interchange to this partner: EDIFACT's
    /// separators with the partner's decimal mark, in its character set.
    pub fn edifact_envelope(
        &self,
        at: NaiveDateTime,
        control: u32,
    ) -> Result<crate::edifact::Envelope> {
        let syntax = crate::edifact::SyntaxId::parse(&self.charset)
            .ok_or_else(|| Error::validation("the partner's character set is not known"))?;
        Ok(crate::edifact::Envelope {
            delimiters: Delimiters {
                decimal: if self.decimal_mark == "," { b',' } else { b'.' },
                ..Delimiters::EDIFACT
            },
            syntax,
            syntax_version: "3".into(),
            sender: self.our_id.clone(),
            sender_qualifier: self.our_qualifier.clone(),
            receiver: self.their_id.clone(),
            receiver_qualifier: self.their_qualifier.clone(),
            at,
            control: format!("{control:08}"),
            test: self.is_test(),
        })
    }

    /// The bytes of a written interchange as this partner wants them: with
    /// a line break after each segment if it asks for that. X12 data never
    /// holds a terminator, so every terminator ends a segment.
    pub fn finish(&self, text: String) -> Vec<u8> {
        if !self.line_breaks {
            return text.into_bytes();
        }
        let terminator = self.segment_terminator.as_str();
        text.replace(terminator, &format!("{terminator}\n"))
            .into_bytes()
    }
}
