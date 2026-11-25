//! Mail: documents sent by email, as the PDF printing makes of them,
//! through the SMTP server each company names.
//!
//! `document.send` renders the document's PDF, records the message on the
//! document (`document_message`: to whom, subject, text, the PDF, who sent
//! it, its Message-ID) and leaves the sending to the outbox: after the
//! command commits, the subscriber `mail` hands the message to the
//! company's server, and the outbox retries it while the server is away,
//! up to [`MAX_ATTEMPTS`]. Who may send what is the company's policy
//! [`SEND`]; by default a draft is not sent.
//!
//! The server's password is set with `smtp_settings.set`, kept in a table
//! of its own and never returned.

use std::sync::Arc;
use std::time::Duration;

use erpcraft_kernel::{
    Command, CommandSpec, Ctx, DynCommand, Error, Event, JsonSchema, Kernel, Migration, Module,
    PolicyPoint, Result, Subscriber, async_trait, command, document, new_id, schemars,
};
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// The policy point asked when a document is sent.
pub const SEND: &str = "document.send";
/// How often a message is tried before it is given up.
pub const MAX_ATTEMPTS: i32 = 8;
/// How long the server has to answer. Short: the outbox waits on it.
const TIMEOUT: Duration = Duration::from_secs(10);
/// Set to `1` to let a company's SMTP server be on this host or a private
/// network: a relay on the same machine, in development.
pub const ALLOW_PRIVATE: &str = "ERPCRAFT_MAIL_ALLOW_PRIVATE";

pub struct MailModule {
    /// Let SMTP servers be on loopback, private and link-local addresses.
    allow_private: bool,
}

impl MailModule {
    /// SMTP servers must be on public addresses, unless the environment
    /// variable `ERPCRAFT_MAIL_ALLOW_PRIVATE` is `1`, read once here.
    pub fn from_env() -> Self {
        MailModule {
            allow_private: std::env::var(ALLOW_PRIVATE).is_ok_and(|v| v == "1"),
        }
    }

    /// Say directly whether private addresses are allowed.
    pub fn allowing_private(allow_private: bool) -> Self {
        MailModule { allow_private }
    }
}

/// Whether `ip` may be connected to without the server allowing private
/// addresses: not loopback, private, link-local, shared, unspecified,
/// broadcast, multicast or unique-local.
fn is_public(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..128).contains(&b)))
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public(IpAddr::V4(v4)),
            None => {
                !(v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local())
            }
        },
    }
}

/// Refuse a server whose name leads to an address the server may not
/// reach: every address it resolves to must be public, unless private ones
/// are allowed.
async fn check_server(
    host: &str,
    port: u16,
    allow_private: bool,
) -> std::result::Result<(), String> {
    if allow_private {
        return Ok(());
    }
    let addresses: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| "the server's name does not resolve".to_owned())?
        .collect();
    if addresses.is_empty() {
        return Err("the server's name does not resolve".into());
    }
    if addresses.iter().any(|a| !is_public(a.ip())) {
        return Err(
            "the server is on this host or a private network, which this installation does not \
             send to"
                .into(),
        );
    }
    Ok(())
}

impl Module for MailModule {
    fn catalogues(&self) -> Vec<(&'static str, &'static str)> {
        vec![("de", include_str!("../i18n/de.json"))]
    }

    fn name(&self) -> &'static str {
        "mail"
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["printing"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "mail",
                sql: include_str!("../migrations/0001_mail.sql"),
            },
            Migration {
                version: 2,
                name: "einvoice",
                sql: include_str!("../migrations/0002_einvoice.sql"),
            },
        ]
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![
            PolicyPoint::new(
                SEND,
                "Asked when a document is sent by email: may this person send this document to \
                 these addresses? Unless configured otherwise a document is sent once it is \
                 posted or approved, and only to the addresses its party has on record. A \
                 company allows other addresses with an allow rule (for example with approval \
                 when `!facts.to_party_only`), allows only some document types, or limits who \
                 sends by a user attribute. Sending also takes the right <doc_type>:send.",
            )
            .text("doc_type", "The document's type, such as sales_invoice")
            .boolean(
                "locked",
                "Whether the document is posted, approved or otherwise final",
            )
            .number(
                "recipients",
                "How many addresses it goes to, copies included",
            )
            .boolean(
                "to_party_only",
                "Whether every address is one the document's party has on record",
            )
            .refuse_when("!facts.locked || !facts.to_party_only")
            .refusal(
                "a draft is not sent, and a document goes only to the addresses its party has on \
                 record unless the company allows otherwise",
            ),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        let allow_private = self.allow_private;
        vec![
            command(SetSettings { allow_private }),
            command(GetSettings),
            command(Send),
            command(Messages),
        ]
    }

    fn subscribers(&self) -> Vec<Arc<dyn Subscriber>> {
        vec![Arc::new(Mailer {
            allow_private: self.allow_private,
        })]
    }
}

// ---------------------------------------------------------------- settings

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SettingsInput {
    /// The company that sends.
    company_id: Uuid,
    /// The SMTP server, such as smtp.example.com.
    host: String,
    /// 587 for STARTTLS, 465 for TLS; the usual one for the security when
    /// omitted.
    port: Option<u16>,
    /// `starttls` (the default), `tls`, or `none` for a relay on the same
    /// host or network, which takes no password.
    security: Option<String>,
    /// The user name to sign in with.
    username: Option<String>,
    /// The password. Kept as given, never returned. Left out: the one on
    /// record stays, unless the host, port, user or security changes,
    /// which clears it; empty: none.
    #[schemars(extend("writeOnly" = true))]
    password: Option<String>,
    /// The address messages come from.
    from_address: String,
    /// The name they come from; the company's when omitted.
    from_name: Option<String>,
    /// Where replies go, when not to the sender.
    reply_to: Option<String>,
    /// An address every message is copied to, unseen: the company's own
    /// record of what it sent.
    bcc: Option<String>,
}

/// A company's SMTP settings, without the password.
#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
struct Settings {
    company_id: Uuid,
    host: String,
    port: i32,
    security: String,
    username: Option<String>,
    from_address: String,
    from_name: Option<String>,
    reply_to: Option<String>,
    bcc: Option<String>,
    /// Whether a password is on record.
    has_password: bool,
}

const SETTINGS: &str = "SELECT s.company_id, s.host, s.port, s.security, s.username,
        s.from_address, s.from_name, s.reply_to, s.bcc,
        EXISTS (SELECT 1 FROM smtp_secret x WHERE x.company_id = s.company_id) AS has_password
    FROM smtp_settings s WHERE s.company_id = $1";

fn address(text: &str, field: &str) -> Result<lettre::Address> {
    text.trim()
        .parse()
        .map_err(|_| Error::field(field, format!("{} is not an email address", text.trim())))
}

fn clean(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

struct SetSettings {
    allow_private: bool,
}

#[async_trait]
impl Command for SetSettings {
    type Input = SettingsInput;
    type Output = Settings;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "smtp_settings.set",
            "Set the SMTP server a company sends documents through: host, port, security, the \
             user and password to sign in with, and the address and name messages come from. \
             The password is kept and never returned; leave it out to keep the one on record. \
             Takes the right to change the company.",
        )
        .label("Email settings")
        .resource("company")
        .target(false)
        .sensitive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: SettingsInput) -> Result<Settings> {
        ctx.require("company:update", input.company_id)?;
        let security = clean(input.security).unwrap_or_else(|| "starttls".into());
        let port = match (input.port, security.as_str()) {
            (Some(port), _) => port,
            (None, "starttls") => 587,
            (None, "tls") => 465,
            (None, "none") => 25,
            _ => {
                return Err(Error::field(
                    "security",
                    "security is starttls, tls or none",
                ));
            }
        };
        if !["starttls", "tls", "none"].contains(&security.as_str()) {
            return Err(Error::field(
                "security",
                "security is starttls, tls or none",
            ));
        }
        let host = input.host.trim().to_owned();
        if host.is_empty() || host.contains(char::is_whitespace) {
            return Err(Error::field("host", "give the server's name"));
        }
        address(&input.from_address, "from_address")?;
        if let Some(reply) = clean(input.reply_to.clone()) {
            address(&reply, "reply_to")?;
        }
        if let Some(bcc) = clean(input.bcc.clone()) {
            address(&bcc, "bcc")?;
        }
        let port_number = port;
        check_server(&host, port_number, self.allow_private)
            .await
            .map_err(|why| Error::field("host", why))?;
        let password = input.password;
        let username = clean(input.username.clone());
        // A password on record is for the server and user it was given
        // for: whoever changes where it goes gives it again.
        let before: Option<(String, i32, String, Option<String>)> = sqlx::query_as(
            "SELECT host, port, security, username FROM smtp_settings WHERE company_id = $1",
        )
        .bind(input.company_id)
        .fetch_optional(&mut *ctx.conn)
        .await?;
        let moved = before.is_some_and(|(h, p, sec, u)| {
            !h.eq_ignore_ascii_case(&host)
                || p != i32::from(port)
                || sec != security
                || u != username
        });
        if moved && password.is_none() {
            sqlx::query("DELETE FROM smtp_secret WHERE company_id = $1")
                .bind(input.company_id)
                .execute(&mut *ctx.conn)
                .await?;
        }
        let keeps_password: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM smtp_secret WHERE company_id = $1)")
                .bind(input.company_id)
                .fetch_one(&mut *ctx.conn)
                .await?;
        let will_have_password = match &password {
            Some(p) => !p.is_empty(),
            None => keeps_password,
        };
        // A password does not go over a connection anyone can read.
        if security == "none" && will_have_password {
            return Err(Error::field(
                "security",
                "a password is sent only over starttls or tls; without encryption, leave the \
                 password empty",
            ));
        }
        sqlx::query(
            "INSERT INTO smtp_settings (company_id, host, port, security, username,
                 from_address, from_name, reply_to, bcc)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (company_id) DO UPDATE SET host = EXCLUDED.host, port = EXCLUDED.port,
                 security = EXCLUDED.security, username = EXCLUDED.username,
                 from_address = EXCLUDED.from_address, from_name = EXCLUDED.from_name,
                 reply_to = EXCLUDED.reply_to, bcc = EXCLUDED.bcc, updated_at = now()",
        )
        .bind(input.company_id)
        .bind(&host)
        .bind(i32::from(port))
        .bind(&security)
        .bind(&username)
        .bind(input.from_address.trim())
        .bind(clean(input.from_name))
        .bind(clean(input.reply_to))
        .bind(clean(input.bcc))
        .execute(&mut *ctx.conn)
        .await?;
        match password {
            Some(p) if p.is_empty() => {
                sqlx::query("DELETE FROM smtp_secret WHERE company_id = $1")
                    .bind(input.company_id)
                    .execute(&mut *ctx.conn)
                    .await?;
            }
            Some(p) => {
                sqlx::query(
                    "INSERT INTO smtp_secret (company_id, password) VALUES ($1, $2)
                     ON CONFLICT (company_id) DO UPDATE SET password = EXCLUDED.password",
                )
                .bind(input.company_id)
                .bind(p)
                .execute(&mut *ctx.conn)
                .await?;
            }
            None => {}
        }
        ctx.touch("company", input.company_id);
        Ok(sqlx::query_as(SETTINGS)
            .bind(input.company_id)
            .fetch_one(&mut *ctx.conn)
            .await?)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CompanyInput {
    /// The company.
    company_id: Uuid,
}

struct GetSettings;

#[async_trait]
impl Command for GetSettings {
    type Input = CompanyInput;
    type Output = Option<Settings>;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "smtp_settings.get",
            "The SMTP server a company sends documents through, without its password: whether \
             one is on record is all that is said. Nothing when none is set. Takes the right \
             to read the company.",
        )
        .label("Email settings")
        .resource("company")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: CompanyInput) -> Result<Option<Settings>> {
        ctx.require("company:read", input.company_id)?;
        Ok(sqlx::query_as(SETTINGS)
            .bind(input.company_id)
            .fetch_optional(&mut *ctx.conn)
            .await?)
    }
}

// -------------------------------------------------------------------- send

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendInput {
    /// Document type, such as `sales_invoice`.
    doc_type: String,
    /// The document.
    id: Uuid,
    /// The addresses it goes to. The contact email the document's party
    /// has on record when omitted (the customer of a sales document, the
    /// vendor of a purchase document).
    to: Option<Vec<String>>,
    /// Addresses it is copied to.
    #[serde(default)]
    cc: Vec<String>,
    /// The subject; the document's type, number and the company's name
    /// when omitted.
    subject: Option<String>,
    /// The text of the message; a short note when omitted.
    message: Option<String>,
    /// The print template the PDF is made with; the default when omitted.
    template_id: Option<Uuid>,
    /// For a sales invoice or credit note: `pdf`, `zugferd` (a ZUGFeRD /
    /// Factur-X PDF), `xrechnung_ubl` or `xrechnung_cii` (the XML alone).
    /// The customer's choice in its party data when omitted. Other
    /// documents go as a PDF.
    format: Option<String>,
}

/// The document types an e-invoice is made of.
const INVOICES: &[&str] = &["sales_invoice", "sales_credit_note"];

/// What a message carries: the file, its name and its media type.
struct Carried {
    bytes: Vec<u8>,
    name: String,
    media_type: String,
    title: String,
}

/// Run a command another module answers, as the caller.
async fn ask(ctx: &mut Ctx<'_>, name: &str, input: Value) -> Result<Value> {
    let handler = ctx
        .kernel
        .registry()
        .command(name)
        .map_err(|_| {
            Error::validation(format!(
                "e-invoices are not installed here ({name} is missing): send a PDF instead"
            ))
        })?
        .handler
        .clone();
    handler.run_json(ctx, input).await
}

/// The file a document is sent as, in `format`.
async fn carried(
    ctx: &mut Ctx<'_>,
    doc_type: &str,
    id: Uuid,
    template_id: Option<Uuid>,
    format: &str,
    buyer_reference: bool,
) -> Result<Carried> {
    use base64::Engine;
    let decode = |data: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(Error::internal)
    };
    // XRechnung needs a buyer reference; without one, the standard alone.
    let profile = if buyer_reference {
        "xrechnung"
    } else {
        "en16931"
    };
    match format {
        "pdf" => {
            let pdf =
                erpcraft_printing::pdf_of(ctx, doc_type, id, template_id, vec![], None).await?;
            Ok(Carried {
                bytes: decode(&pdf.data)?,
                name: pdf.filename,
                media_type: "application/pdf".into(),
                title: pdf.title,
            })
        }
        "zugferd" => {
            let out = ask(
                ctx,
                "einvoice.export_pdf",
                json!({ "doc_type": doc_type, "id": id, "profile": profile }),
            )
            .await?;
            Ok(Carried {
                bytes: decode(out["data"].as_str().unwrap_or_default())?,
                name: out["filename"].as_str().unwrap_or("invoice.pdf").to_owned(),
                media_type: "application/pdf".into(),
                title: out["title"].as_str().unwrap_or_default().to_owned(),
            })
        }
        "xrechnung_ubl" | "xrechnung_cii" => {
            let syntax = if format == "xrechnung_ubl" {
                "ubl"
            } else {
                "cii"
            };
            let out = ask(
                ctx,
                "einvoice.export",
                json!({ "doc_type": doc_type, "id": id, "syntax": syntax, "profile": profile }),
            )
            .await?;
            if out["valid"] != true {
                return Err(Error::validation(
                    "the e-invoice does not hold; see einvoice.export for what it lacks",
                ));
            }
            let number: Option<String> =
                sqlx::query_scalar("SELECT number FROM document WHERE id = $1")
                    .bind(id)
                    .fetch_one(&mut *ctx.conn)
                    .await?;
            Ok(Carried {
                bytes: out["xml"].as_str().unwrap_or_default().as_bytes().to_vec(),
                name: out["filename"].as_str().unwrap_or("invoice.xml").to_owned(),
                media_type: "application/xml".into(),
                title: format!("Invoice {}", number.unwrap_or_default()),
            })
        }
        other => Err(Error::field(
            "format",
            format!("{other} is not pdf, zugferd, xrechnung_ubl or xrechnung_cii"),
        )),
    }
}

/// A message a document was sent with, without its PDF.
#[derive(Debug, Serialize, JsonSchema, sqlx::FromRow)]
pub struct MessageView {
    pub id: Uuid,
    pub document_id: Uuid,
    pub doc_type: String,
    pub to_addresses: Vec<String>,
    pub cc_addresses: Vec<String>,
    pub subject: String,
    pub body: String,
    pub attachment_name: String,
    /// `pdf`, `zugferd`, `xrechnung_ubl` or `xrechnung_cii`.
    pub format: String,
    pub attachment_type: String,
    /// `queued`, `sent` or `failed`.
    pub status: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    /// The Message-ID it left with.
    pub message_id: String,
    pub sent_by: Uuid,
    pub sent_by_name: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub sent_at: Option<chrono::DateTime<chrono::Utc>>,
}

const MESSAGE: &str = "SELECT m.id, m.document_id, m.doc_type, m.to_addresses, m.cc_addresses,
        m.subject, m.body, m.attachment_name, m.format, m.attachment_type, m.status, m.attempts, m.last_error, m.message_id,
        m.sent_by, u.name AS sent_by_name, m.created_at, m.sent_at
    FROM document_message m LEFT JOIN app_user u ON u.id = m.sent_by";

/// The addresses the party of a document has on record: its contact's and
/// its electronic address when that is an email.
async fn party_addresses(ctx: &mut Ctx<'_>, doc_type: &str, id: Uuid) -> Result<Vec<String>> {
    let Ok(entry) = ctx.kernel.registry().command("document.parties") else {
        return Ok(vec![]);
    };
    let handler = entry.handler.clone();
    let parties = handler
        .run_json(ctx, json!({ "doc_type": doc_type, "id": id }))
        .await
        .unwrap_or(Value::Null);
    // The party that is not the company.
    let party = ["buyer", "seller"]
        .iter()
        .map(|role| &parties[role])
        .find(|p| p.is_object() && p["kind"] != "company")
        .cloned()
        .unwrap_or(Value::Null);
    let mut found = vec![];
    for field in ["contact_email", "electronic_address"] {
        if let Some(text) = party[field].as_str()
            && text.parse::<lettre::Address>().is_ok()
            && !found.iter().any(|f: &String| f.eq_ignore_ascii_case(text))
        {
            found.push(text.to_owned());
        }
    }
    Ok(found)
}

struct Send;

#[async_trait]
impl Command for Send {
    type Input = SendInput;
    type Output = MessageView;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "document.send",
            "Send a document by email as a PDF (the one document.render_pdf makes), to the \
             contact email its party has on record unless other addresses are given, through \
             the company's SMTP server (smtp_settings.set). The message is recorded on the \
             document and leaves once the command commits; the outbox tries again while the \
             server cannot be reached. The company's policy document.send decides who may send \
             what; by default a draft is not sent, and a document goes only to its party's \
             addresses on record. Takes the right <doc_type>:send, such as sales_invoice:send, \
             besides the right to read the document.",
        )
        .label("Send by email")
        .resource("document")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: SendInput) -> Result<MessageView> {
        // The one path by which a document is read: its permissions hold.
        let view = document::get(ctx, &input.doc_type, input.id).await?;
        let company_id = view.header.company_id;
        // Reading a document is not sending it.
        ctx.require(&format!("{}:send", input.doc_type), company_id)?;
        let company: String = sqlx::query_scalar("SELECT name FROM company WHERE id = $1")
            .bind(company_id)
            .fetch_one(&mut *ctx.conn)
            .await?;
        let settings: Option<Settings> = sqlx::query_as(SETTINGS)
            .bind(company_id)
            .fetch_optional(&mut *ctx.conn)
            .await?;
        if settings.is_none() {
            return Err(Error::validation(
                "this company has no email settings: set its SMTP server with smtp_settings.set",
            ));
        }
        let on_record = party_addresses(ctx, &input.doc_type, input.id).await?;
        let to: Vec<String> = match input.to {
            Some(to) => to
                .into_iter()
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
                .collect(),
            None => on_record.iter().take(1).cloned().collect(),
        };
        if to.is_empty() {
            return Err(Error::field(
                "to",
                "the party has no email address on record: give the addresses to send to",
            ));
        }
        let cc: Vec<String> = input
            .cc
            .into_iter()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect();
        for a in &to {
            address(a, "to")?;
        }
        for a in &cc {
            address(a, "cc")?;
        }
        if to.len() + cc.len() > 20 {
            return Err(Error::field(
                "to",
                "a document goes to at most 20 addresses",
            ));
        }
        let to_party_only = to
            .iter()
            .chain(&cc)
            .all(|a| on_record.iter().any(|r| r.eq_ignore_ascii_case(a)));
        let locked: bool = sqlx::query_scalar("SELECT locked FROM document WHERE id = $1")
            .bind(input.id)
            .fetch_one(&mut *ctx.conn)
            .await?;
        ctx.decide(
            SEND,
            company_id,
            json!({
                "doc_type": input.doc_type,
                "locked": locked,
                "recipients": (to.len() + cc.len()).to_string(),
                "to_party_only": to_party_only,
            }),
        )
        .await?;

        // An invoice goes as its customer wants it; anything else as a PDF.
        let invoice = INVOICES.contains(&input.doc_type.as_str());
        let customer = view.header.summary["customer_id"]
            .as_str()
            .and_then(|c| Uuid::parse_str(c).ok());
        let format = match (clean(input.format), invoice, customer) {
            (Some(format), true, _) => format,
            (Some(format), false, _) if format == "pdf" => format,
            (Some(_), false, _) => {
                return Err(Error::field(
                    "format",
                    "only a sales invoice or credit note is sent as an e-invoice",
                ));
            }
            (None, true, Some(customer)) => {
                erpcraft_party::einvoice_format(&mut *ctx.conn, customer).await?
            }
            (None, _, _) => "pdf".into(),
        };
        let buyer_reference = match customer {
            Some(customer) => {
                sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS (SELECT 1 FROM party_profile WHERE party_type = 'customer'
                                AND party_id = $1 AND invoice_reference IS NOT NULL)",
                )
                .bind(customer)
                .fetch_one(&mut *ctx.conn)
                .await?
            }
            None => false,
        };
        let file = carried(
            ctx,
            &input.doc_type,
            input.id,
            input.template_id,
            &format,
            buyer_reference,
        )
        .await?;
        let subject =
            clean(input.subject).unwrap_or_else(|| format!("{} from {company}", file.title));
        let body = clean(input.message).unwrap_or_else(|| {
            format!(
                "Please find attached {}.\n\nKind regards,\n{company}\n",
                file.title
            )
        });
        let id = new_id();
        let message_id = format!("<{id}@erpcraft>");
        sqlx::query(
            "INSERT INTO document_message (id, company_id, document_id, doc_type, to_addresses,
                 cc_addresses, subject, body, attachment_name, attachment, message_id, sent_by,
                 format, attachment_type)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(id)
        .bind(company_id)
        .bind(input.id)
        .bind(&input.doc_type)
        .bind(&to)
        .bind(&cc)
        .bind(&subject)
        .bind(&body)
        .bind(&file.name)
        .bind(&file.bytes)
        .bind(&message_id)
        .bind(ctx.actor.user_id)
        .bind(&format)
        .bind(&file.media_type)
        .execute(&mut *ctx.conn)
        .await?;
        ctx.touch("document", input.id);
        Ok(sqlx::query_as(&format!("{MESSAGE} WHERE m.id = $1"))
            .bind(id)
            .fetch_one(&mut *ctx.conn)
            .await?)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MessagesInput {
    /// Document type.
    doc_type: String,
    /// The document.
    id: Uuid,
}

#[derive(Serialize, JsonSchema)]
struct MessagesOutput {
    /// The messages the document was sent with, the latest first.
    items: Vec<MessageView>,
}

struct Messages;

#[async_trait]
impl Command for Messages {
    type Input = MessagesInput;
    type Output = MessagesOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "document.messages",
            "The emails a document was sent with: to whom, the subject and text, who sent it, \
             when it left, the Message-ID, and whether it is still waiting or was given up. \
             The server's own answer to a failure shows only to someone who may change the \
             company. Takes the right to read the document.",
        )
        .label("Sent")
        .resource("document")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: MessagesInput) -> Result<MessagesOutput> {
        let view = document::get(ctx, &input.doc_type, input.id).await?;
        let mut items: Vec<MessageView> = sqlx::query_as(&format!(
            "{MESSAGE} WHERE m.document_id = $1 ORDER BY m.created_at DESC"
        ))
        .bind(input.id)
        .fetch_all(&mut *ctx.conn)
        .await?;
        // What the server answered can say where it is and how it is set
        // up: for whoever sets it up, not for every reader of the document.
        if !ctx.actor.can("company:update", view.header.company_id) {
            for item in &mut items {
                if item.last_error.is_some() {
                    item.last_error = Some("the server did not take it".into());
                }
            }
        }
        Ok(MessagesOutput { items })
    }
}

// --------------------------------------------------------------- the mailer

#[derive(sqlx::FromRow)]
struct Outgoing {
    company_id: Uuid,
    to_addresses: Vec<String>,
    cc_addresses: Vec<String>,
    subject: String,
    body: String,
    attachment_name: String,
    attachment: Vec<u8>,
    attachment_type: String,
    message_id: String,
    status: String,
    attempts: i32,
}

#[derive(sqlx::FromRow)]
struct Server {
    host: String,
    port: i32,
    security: String,
    username: Option<String>,
    password: Option<String>,
    from_address: String,
    from_name: Option<String>,
    reply_to: Option<String>,
    bcc: Option<String>,
    company: String,
}

/// Hands the messages `document.send` recorded to their company's server.
struct Mailer {
    allow_private: bool,
}

#[async_trait]
impl Subscriber for Mailer {
    fn name(&self) -> &'static str {
        "mail"
    }

    fn wants(&self, topic: &str) -> bool {
        topic == "document.send"
    }

    async fn handle(&self, kernel: &Kernel, event: &Event) -> Result<()> {
        let Some(id) = event.payload["output"]["id"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return Ok(());
        };
        let outgoing: Option<Outgoing> = sqlx::query_as(
            "SELECT company_id, to_addresses, cc_addresses, subject, body, attachment_name,
                    attachment, attachment_type, message_id, status, attempts
             FROM document_message WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(kernel.pool())
        .await?;
        let Some(outgoing) = outgoing else {
            return Ok(());
        };
        if outgoing.status != "queued" {
            return Ok(());
        }
        let outcome = send(kernel, &outgoing, self.allow_private).await;
        let attempts = outgoing.attempts + 1;
        match outcome {
            Ok(()) => {
                sqlx::query(
                    "UPDATE document_message SET status = 'sent', attempts = $2, sent_at = now(),
                         last_error = NULL
                     WHERE id = $1",
                )
                .bind(id)
                .bind(attempts)
                .execute(kernel.pool())
                .await?;
                Ok(())
            }
            Err(error) => {
                let given_up = attempts >= MAX_ATTEMPTS;
                sqlx::query(
                    "UPDATE document_message SET attempts = $2, last_error = $3,
                         status = CASE WHEN $4 THEN 'failed' ELSE status END
                     WHERE id = $1",
                )
                .bind(id)
                .bind(attempts)
                .bind(&error)
                .bind(given_up)
                .execute(kernel.pool())
                .await?;
                tracing::warn!(message = %id, error, "a document's email was not sent");
                if given_up {
                    Ok(())
                } else {
                    Err(Error::internal(format!("the email was not sent: {error}")))
                }
            }
        }
    }
}

/// Send one message through its company's server. The reason as text when
/// it did not go.
async fn send(
    kernel: &Kernel,
    outgoing: &Outgoing,
    allow_private: bool,
) -> std::result::Result<(), String> {
    let server: Option<Server> = sqlx::query_as(
        "SELECT s.host, s.port, s.security, s.username, x.password, s.from_address,
                s.from_name, s.reply_to, s.bcc, c.name AS company
         FROM smtp_settings s JOIN company c ON c.id = s.company_id
         LEFT JOIN smtp_secret x ON x.company_id = s.company_id
         WHERE s.company_id = $1",
    )
    .bind(outgoing.company_id)
    .fetch_optional(kernel.pool())
    .await
    .map_err(|e| e.to_string())?;
    let server = server.ok_or("the company has no email settings")?;
    let mailbox = |text: &str| -> std::result::Result<Mailbox, String> {
        text.parse::<lettre::Address>()
            .map(|a| Mailbox::new(None, a))
            .map_err(|_| format!("{text} is not an email address"))
    };
    let from = Mailbox::new(
        Some(server.from_name.clone().unwrap_or(server.company.clone())),
        server
            .from_address
            .parse()
            .map_err(|_| "the sender's address is not valid".to_owned())?,
    );
    let mut builder = Message::builder()
        .from(from)
        .subject(outgoing.subject.clone())
        .message_id(Some(outgoing.message_id.clone()));
    for to in &outgoing.to_addresses {
        builder = builder.to(mailbox(to)?);
    }
    for cc in &outgoing.cc_addresses {
        builder = builder.cc(mailbox(cc)?);
    }
    if let Some(reply) = &server.reply_to {
        builder = builder.reply_to(mailbox(reply)?);
    }
    if let Some(bcc) = &server.bcc {
        builder = builder.bcc(mailbox(bcc)?);
    }
    let pdf = ContentType::parse(&outgoing.attachment_type).map_err(|e| e.to_string())?;
    let message = builder
        .multipart(
            MultiPart::mixed()
                .singlepart(SinglePart::plain(outgoing.body.clone()))
                .singlepart(
                    Attachment::new(outgoing.attachment_name.clone())
                        .body(outgoing.attachment.clone(), pdf),
                ),
        )
        .map_err(|e| e.to_string())?;
    let port = u16::try_from(server.port).map_err(|_| "the port is not valid".to_owned())?;
    // Checked again now: what a name resolves to can change.
    check_server(&server.host, port, allow_private).await?;
    let transport = match server.security.as_str() {
        "tls" => AsyncSmtpTransport::<Tokio1Executor>::relay(&server.host),
        "starttls" => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&server.host),
        _ => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
            &server.host,
        )),
    }
    .map_err(|e| e.to_string())?
    .port(port)
    .timeout(Some(TIMEOUT));
    let transport = match (&server.username, &server.password) {
        (Some(user), Some(password)) if server.security != "none" => {
            transport.credentials(Credentials::new(user.clone(), password.clone()))
        }
        _ => transport,
    }
    .build();
    transport
        .send(message)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_are_public() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.1",
            "172.16.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in ["203.0.114.1", "8.8.8.8", "2a00:1450::1"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[tokio::test]
    async fn a_server_on_this_host_is_refused_unless_allowed() {
        assert!(check_server("127.0.0.1", 25, false).await.is_err());
        assert!(check_server("localhost", 25, false).await.is_err());
        assert!(check_server("127.0.0.1", 25, true).await.is_ok());
    }
}
