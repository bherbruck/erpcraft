//! The template engine and what it is allowed to do.
//!
//! Templates are written by administrators and render data the caller may
//! read. The engine is MiniJinja, set up so that a template:
//!
//! - cannot read a file or reach the network: the crate is built without
//!   its loader, and without `include`, `import`, `extends` and macros, so
//!   a template is one source that names no other;
//! - cannot run code beyond the template language: there is no way to call
//!   into the server but the filters registered here, which only format
//!   values;
//! - is bounded: every instruction, every byte it produces or copies and
//!   every item it visits costs fuel, and a render that runs out stops
//!   with an error; recursion is limited; and the output is capped on top;
//! - escapes for HTML whatever it prints, with no way to turn that off:
//!   the `safe` filter is removed and `{% autoescape %}` is refused;
//! - produces no active content: see [`check_html`], which runs on the
//!   source when a template is saved and on the output of every render.

use std::io;
use std::str::FromStr;

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, NaiveDate, Utc};
use minijinja::{
    AutoEscape, Environment, Error as TemplateError, ErrorKind, State, UndefinedBehavior, Value,
};
use rust_decimal::{Decimal, RoundingStrategy};

/// What one render may spend. One fuel pays for one instruction or for 64
/// bytes produced; a page of a few hundred lines needs a few tens of
/// thousands.
pub(crate) const FUEL: u64 = 2_000_000;
/// The most a rendered page may weigh.
pub(crate) const MAX_OUTPUT: usize = 4 * 1024 * 1024;
/// The most a template may weigh.
pub(crate) const MAX_SOURCE: usize = 256 * 1024;
const RECURSION_LIMIT: usize = 50;

/// The environment every template is compiled and rendered in.
pub(crate) fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_fuel(Some(FUEL));
    env.set_recursion_limit(RECURSION_LIMIT);
    env.set_auto_escape_callback(|_| AutoEscape::Html);
    // A reference that cannot be read prints as nothing, however deep the
    // template looks into it.
    env.set_undefined_behavior(UndefinedBehavior::Chainable);
    env.remove_filter("safe");
    env.remove_global("debug");
    env.add_filter("money", money);
    env.add_filter("price", price);
    env.add_filter("decimal", decimal);
    env.add_filter("mul", mul);
    env.add_filter("date", date);
    env.add_filter("ref", reference);
    env.add_filter("show", show);
    env.add_filter("lots", lots);
    env
}

/// Why a template cannot be used, in words for whoever wrote it.
pub(crate) fn check_source(source: &str) -> Result<(), String> {
    if source.len() > MAX_SOURCE {
        return Err(format!(
            "the template is longer than {} KiB",
            MAX_SOURCE / 1024
        ));
    }
    if has_autoescape_block(source) {
        return Err("{% autoescape %} is not allowed: everything printed is escaped".into());
    }
    check_html(source)?;
    let env = environment();
    env.template_from_str(source)
        .map(|_| ())
        .map_err(|error| describe(&error))
}

/// Render `source` with `context`, within the bounds, sanitize what it
/// made, and wrap it into the page: the template writes the page's body;
/// the head, with the title and the one stylesheet, is the server's.
pub(crate) fn render(source: &str, context: &serde_json::Value) -> Result<String, String> {
    if has_autoescape_block(source) {
        return Err("{% autoescape %} is not allowed: everything printed is escaped".into());
    }
    let env = environment();
    let template = env
        .template_from_str(source)
        .map_err(|error| describe(&error))?;
    let mut out = Capped::default();
    template
        .render_captured_to(Value::from(minijinja::value::Serde(context)), &mut out)
        .map_err(|error| {
            if out.full {
                format!("the page is larger than {} MiB", MAX_OUTPUT / 1024 / 1024)
            } else {
                describe(&error)
            }
        })?;
    let html = String::from_utf8(out.buf).map_err(|_| "the page is not valid text".to_owned())?;
    let title = context
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    Ok(page(title, &sanitize(&html)))
}

/// The page around a template's body: a fixed head with the title and the
/// built-in stylesheet. Nothing of the head comes from a template, so no
/// template can add a script, a stylesheet or a `<meta>` to it.
fn page(title: &str, body: &str) -> String {
    let title = title
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{title}</title>\n<style>\n{}\n</style>\n</head>\n<body>\n{body}\n</body>\n</html>\n",
        crate::builtin::STYLE
    )
}

/// The elements a printed document may have.
const ELEMENTS: &[&str] = &[
    "a",
    "abbr",
    "address",
    "article",
    "b",
    "blockquote",
    "br",
    "caption",
    "cite",
    "code",
    "col",
    "colgroup",
    "dd",
    "del",
    "div",
    "dl",
    "dt",
    "em",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "i",
    "img",
    "ins",
    "li",
    "main",
    "mark",
    "ol",
    "p",
    "pre",
    "q",
    "s",
    "section",
    "small",
    "span",
    "strong",
    "sub",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "time",
    "tr",
    "u",
    "ul",
];

/// The CSS properties a `style` attribute may set: layout and type. None
/// of them can load a URL (no `background`, `list-style`, `border-image`,
/// `content`, `cursor`).
const STYLE_PROPERTIES: &[&str] = &[
    "text-align",
    "vertical-align",
    "font-weight",
    "font-style",
    "font-size",
    "font-family",
    "font-variant-numeric",
    "line-height",
    "letter-spacing",
    "text-transform",
    "text-decoration",
    "white-space",
    "color",
    "background-color",
    "width",
    "min-width",
    "max-width",
    "height",
    "min-height",
    "margin",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "padding",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "border",
    "border-top",
    "border-right",
    "border-bottom",
    "border-left",
    "border-color",
    "border-style",
    "border-width",
    "border-collapse",
    "border-spacing",
    "display",
    "float",
    "clear",
    "page-break-before",
    "page-break-after",
    "page-break-inside",
    "break-before",
    "break-after",
    "break-inside",
];

/// Of relative URLs, only a link within the page (`#totals`) stays: the
/// page has no address of its own that a relative path could mean.
fn fragment_only(url: &str) -> Option<std::borrow::Cow<'_, str>> {
    url.starts_with('#')
        .then_some(std::borrow::Cow::Borrowed(url))
}

/// Keep only what a printed document needs, by a real HTML5 parser
/// (`ammonia`, on `html5ever`): the elements of [`ELEMENTS`], the
/// attributes `class`, `style` (with [`STYLE_PROPERTIES`] only), `title`,
/// `lang` and `dir`, cell spans, image sources and link targets that are
/// `http`, `https`, `mailto` or a `#fragment`. Scripts, event handlers,
/// frames, objects, forms, `base`, `meta`, `link`, `style`, `svg` and
/// `math` go, and so does the content of `script`, `style`, `title`,
/// `textarea` and the like, which a browser would not show as text.
pub(crate) fn sanitize(html: &str) -> String {
    use std::collections::HashSet;
    let mut builder = ammonia::Builder::empty();
    builder
        .add_tags(ELEMENTS)
        .add_generic_attributes(&["class", "style", "title", "lang", "dir"])
        .add_tag_attributes("a", &["href"])
        .add_tag_attributes("img", &["src", "alt", "width", "height"])
        .add_tag_attributes("td", &["colspan", "rowspan"])
        .add_tag_attributes("th", &["colspan", "rowspan", "scope"])
        .add_tag_attributes("col", &["span"])
        .add_tag_attributes("colgroup", &["span"])
        .add_tag_attributes("ol", &["start"])
        .add_tag_attributes("time", &["datetime"])
        .url_schemes(HashSet::from(["http", "https", "mailto"]))
        .url_relative(ammonia::UrlRelative::Custom(Box::new(fragment_only)))
        .link_rel(Some("noopener noreferrer"))
        .clean_content_tags(HashSet::from([
            "script",
            "style",
            "title",
            "textarea",
            "noscript",
            "template",
            "xmp",
            "noembed",
            "noframes",
            "plaintext",
            "iframe",
            "object",
            "svg",
            "math",
            "select",
        ]))
        .strip_comments(true)
        .filter_style_properties(STYLE_PROPERTIES.iter().copied().collect());
    builder.clean(html).to_string()
}

/// A template error as one line: what, and where.
fn describe(error: &TemplateError) -> String {
    let what = match error.kind() {
        ErrorKind::OutOfFuel => {
            "the template does too much work and was stopped (a loop that runs too long?)"
                .to_owned()
        }
        _ => error
            .detail()
            .map(str::to_owned)
            .unwrap_or_else(|| error.kind().to_string()),
    };
    match error.line() {
        Some(line) => format!("{what} (line {line})"),
        None => what,
    }
}

fn has_autoescape_block(source: &str) -> bool {
    let mut rest = source;
    while let Some(at) = rest.find("{%") {
        let after = rest[at + 2..]
            .trim_start_matches(['-', '+', '~'])
            .trim_start();
        if after.starts_with("autoescape") {
            return true;
        }
        rest = &rest[at + 2..];
    }
    false
}

/// A writer that refuses past [`MAX_OUTPUT`].
#[derive(Default)]
struct Capped {
    buf: Vec<u8>,
    full: bool,
}

impl io::Write for Capped {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.buf.len() + bytes.len() > MAX_OUTPUT {
            self.full = true;
            return Err(io::Error::other("the page is too large"));
        }
        self.buf.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// ------------------------------------------------------------- active HTML

/// Elements that run code or load another page into this one.
const REFUSED_ELEMENTS: &[&str] = &[
    "script", "iframe", "frame", "frameset", "object", "embed", "applet", "base", "portal", "form",
    "link", "style", "svg", "math", "textarea", "title", "head", "html", "body",
];
/// URL schemes that run code when followed or loaded.
const REFUSED_SCHEMES: &[&str] = &["javascript:", "vbscript:", "data:text/html"];

/// At save time only, to tell whoever wrote a template what will not
/// work: safety rests on [`sanitize`], which every rendered page goes
/// through. Refuse what would run in the application's origin if the page
/// were opened there: `<script>` and the elements that embed or redirect,
/// `<meta http-equiv>`, event-handler attributes (`on...=`), and
/// `javascript:` URLs in any attribute. Text between tags is not looked
/// at: what the template prints there is escaped, and markup in a
/// template's own text is a tag like any other.
pub(crate) fn check_html(html: &str) -> Result<(), String> {
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if html[i..].starts_with("<!--") {
            i = html[i..].find("-->").map_or(bytes.len(), |end| i + end + 3);
            continue;
        }
        let (tag, end) = parse_tag(html, i + 1);
        i = end;
        let Some(tag) = tag else { continue };
        if REFUSED_ELEMENTS.contains(&tag.name.as_str()) {
            let why = match tag.name.as_str() {
                "style" | "link" | "head" | "html" | "body" | "title" => {
                    ": a template writes the page's body; the head and its stylesheet are the \
                     server's. Style with the classes of the built-in templates and style= \
                     attributes"
                }
                _ => "",
            };
            return Err(format!(
                "contains <{}>, which is not allowed{why}",
                tag.name
            ));
        }
        for (name, value) in &tag.attributes {
            if name.starts_with("on") {
                return Err(format!(
                    "contains the event handler attribute {name}=, which is not allowed"
                ));
            }
            if tag.name == "meta" && name == "http-equiv" {
                return Err("contains <meta http-equiv>, which is not allowed".into());
            }
            let url: String = decode_entities(value)
                .chars()
                .filter(|c| !c.is_whitespace() && !c.is_control())
                .collect::<String>()
                .to_ascii_lowercase();
            if let Some(scheme) = REFUSED_SCHEMES.iter().find(|s| url.starts_with(**s)) {
                return Err(format!(
                    "contains a {} URL in {name}=, which is not allowed",
                    scheme.trim_end_matches(':')
                ));
            }
            // CSS can load a URL too.
            if name == "style" && url.contains("javascript:") {
                return Err("contains a javascript: URL in style=, which is not allowed".into());
            }
        }
    }
    Ok(())
}

struct Tag {
    name: String,
    attributes: Vec<(String, String)>,
}

/// Parse the tag that starts after `<` at `start`. Returns the tag, or
/// `None` for something that is not one (`a < b`, `</p>`, `<!doctype>`),
/// and where scanning goes on.
fn parse_tag(html: &str, start: usize) -> (Option<Tag>, usize) {
    let bytes = html.as_bytes();
    let mut i = start;
    if i < bytes.len() && bytes[i] == b'/' {
        i += 1;
    }
    let name_start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
        i += 1;
    }
    if i == name_start || !bytes[name_start].is_ascii_alphabetic() {
        return (None, start);
    }
    let closing = start < bytes.len() && bytes[start] == b'/';
    let name = html[name_start..i].to_ascii_lowercase();
    let mut attributes = Vec::new();
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'>' {
            i += 1;
            break;
        }
        let attr_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && !matches!(bytes[i], b'=' | b'>' | b'/')
        {
            i += 1;
        }
        if i == attr_start {
            // A stray character such as a quote: step over it.
            i += 1;
            continue;
        }
        let attr = html[attr_start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && matches!(bytes[i], b'"' | b'\'') {
                let quote = bytes[i];
                let value_start = i + 1;
                let end = html[value_start..]
                    .bytes()
                    .position(|b| b == quote)
                    .map_or(bytes.len(), |p| value_start + p);
                value = html[value_start..end].to_owned();
                i = (end + 1).min(bytes.len());
            } else {
                let value_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                value = html[value_start..i].to_owned();
            }
        }
        attributes.push((attr, value));
    }
    if closing {
        attributes.clear();
    }
    (Some(Tag { name, attributes }), i)
}

/// Decode the character references a browser would decode in an
/// attribute, so that `jav&#x61;script:` is seen for what it is.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest
            .char_indices()
            .take(12)
            .find(|(_, c)| *c == ';')
            .map(|(i, _)| i);
        let entity = end.map(|end| &rest[1..end]);
        let decoded = entity.and_then(|entity| {
            if let Some(hex) = entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
            {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = entity.strip_prefix('#') {
                dec.parse().ok().and_then(char::from_u32)
            } else {
                match entity.to_ascii_lowercase().as_str() {
                    "colon" => Some(':'),
                    "tab" => Some('\t'),
                    "newline" => Some('\n'),
                    "amp" => Some('&'),
                    _ => None,
                }
            }
        });
        match (decoded, end) {
            (Some(c), Some(end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ------------------------------------------------------------------ filters

fn invalid(message: impl Into<String>) -> TemplateError {
    TemplateError::new(ErrorKind::InvalidOperation, message.into())
}

/// A value as an exact decimal: from its text, never through a float.
/// `None` for nothing (null, undefined, empty text).
fn to_decimal(value: &Value) -> Result<Option<Decimal>, TemplateError> {
    if value.is_undefined() || value.is_none() {
        return Ok(None);
    }
    let text = match value.as_str() {
        Some(text) => text.trim().to_owned(),
        None => value.to_string(),
    };
    if text.is_empty() {
        return Ok(None);
    }
    Decimal::from_str(&text)
        .or_else(|_| Decimal::from_scientific(&text))
        .map(Some)
        .map_err(|_| invalid(format!("'{text}' is not a number")))
}

/// `1234567.891` at 2 places as `1,234,567.89`; at `None` places with as
/// many as it has, without trailing zeros.
pub(crate) fn format_decimal(value: Decimal, places: Option<u32>) -> String {
    let text = match places {
        Some(places) => {
            let rounded =
                value.round_dp_with_strategy(places, RoundingStrategy::MidpointAwayFromZero);
            format!("{rounded:.prec$}", prec = places as usize)
        }
        None => value.normalize().to_string(),
    };
    let (sign, digits) = match text.strip_prefix('-') {
        Some(digits) => ("-", digits),
        None => ("", text.as_str()),
    };
    let (whole, fraction) = match digits.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (digits, None),
    };
    let mut grouped = String::with_capacity(whole.len() + whole.len() / 3);
    for (i, digit) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    // "-0.00" says nothing a reader wants.
    let negative_zero = sign == "-" && digits.chars().all(|c| c == '0' || c == '.');
    let sign = if negative_zero { "" } else { sign };
    match fraction {
        Some(fraction) => format!("{sign}{grouped}.{fraction}"),
        None => format!("{sign}{grouped}"),
    }
}

/// The decimal places of the company's currency, as the context has it.
fn currency_places(state: &State) -> u32 {
    state
        .lookup("company")
        .and_then(|company| company.get_attr("decimals").ok())
        .and_then(|places| u32::try_from(places).ok())
        .unwrap_or(2)
}

/// `{{ amount | money }}`: with the decimal places of the company's
/// currency, or `money(0)` for another number of places.
fn money(state: &State, value: Value, places: Option<u32>) -> Result<String, TemplateError> {
    let places = places.unwrap_or_else(|| currency_places(state)).min(10);
    Ok(to_decimal(&value)?
        .map(|value| format_decimal(value, Some(places)))
        .unwrap_or_default())
}

/// `{{ line.unit_price | price }}`: a price of one unit, with at least
/// the places of the company's currency and at most six.
fn price(state: &State, value: Value) -> Result<String, TemplateError> {
    let currency = currency_places(state);
    Ok(to_decimal(&value)?
        .map(|value| {
            let places = value.normalize().scale().clamp(currency, 6);
            format_decimal(value, Some(places))
        })
        .unwrap_or_default())
}

/// `{{ line.qty | mul(line.unit_price) }}`: the exact product, as a
/// number to format with `money`. Nothing if either is missing.
fn mul(value: Value, other: Value) -> Result<String, TemplateError> {
    match (to_decimal(&value)?, to_decimal(&other)?) {
        (Some(a), Some(b)) => a
            .checked_mul(b)
            .map(|product| product.to_string())
            .ok_or_else(|| invalid("the product is too large")),
        _ => Ok(String::new()),
    }
}

/// `{{ qty | decimal }}` without trailing zeros, or `decimal(3)` with
/// exactly three places.
fn decimal(value: Value, places: Option<u32>) -> Result<String, TemplateError> {
    Ok(to_decimal(&value)?
        .map(|value| format_decimal(value, places.map(|p| p.min(20))))
        .unwrap_or_default())
}

/// `{{ doc.doc_date | date }}` as `31 Mar 2026`, or `date("%Y-%m-%d")`.
fn date(value: Value, format: Option<String>) -> Result<String, TemplateError> {
    let Some(text) = value.as_str().map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(String::new());
    };
    // Day, month in letters, year: read the same everywhere, which a
    // numeric date is not (03/04 is March or April). There is no locale
    // on a company yet to choose otherwise.
    let format = format.unwrap_or_else(|| "%-d %b %Y".into());
    let items: Vec<Item> = StrftimeItems::new(&format).collect();
    if items.iter().any(|item| matches!(item, Item::Error)) {
        return Err(invalid(format!("'{format}' is not a date format")));
    }
    if let Ok(day) = NaiveDate::from_str(text) {
        return Ok(day.format_with_items(items.into_iter()).to_string());
    }
    match DateTime::parse_from_rfc3339(text) {
        Ok(at) => Ok(at
            .with_timezone(&Utc)
            .format_with_items(items.into_iter())
            .to_string()),
        Err(_) => Err(invalid(format!("'{text}' is not a date"))),
    }
}

/// `{{ line.item_id | ref }}`: what tells a person which record an id
/// refers to (`WIDGET Widget`), or nothing if the caller may not read it.
/// `ref("short")` gives only its first part, such as the code, for a
/// narrow column.
fn reference(state: &State, value: Value, form: Option<String>) -> String {
    let Some(id) = value.as_str() else {
        return String::new();
    };
    let found = state.lookup("refs").and_then(|refs| refs.get_attr(id).ok());
    let text = |key: &str| {
        found
            .as_ref()
            .and_then(|found| found.get_attr(key).ok())
            .and_then(|text| text.as_str().map(str::to_owned))
            .filter(|text| !text.is_empty())
    };
    let short = form.as_deref() == Some("short");
    (if short { text("short") } else { None })
        .or_else(|| text("display"))
        .unwrap_or_default()
}

/// `{{ line.lots | lots }}`: the lots or serial numbers of a line, as
/// `F-1 × 6, F-2 × 4`; a serial number (one unit) without its quantity.
fn lots(value: Value) -> Result<String, TemplateError> {
    let Ok(items) = value.try_iter() else {
        return Ok(String::new());
    };
    let mut shown = vec![];
    for item in items {
        let Ok(lot) = item.get_attr("lot") else {
            continue;
        };
        let Some(lot) = lot.as_str().map(str::to_owned) else {
            continue;
        };
        let qty = item
            .get_attr("qty")
            .ok()
            .map(|q| to_decimal(&q))
            .transpose()?
            .flatten();
        shown.push(match qty {
            Some(qty) if qty != Decimal::ONE => {
                format!("{lot} × {}", format_decimal(qty.normalize(), None))
            }
            _ => lot,
        });
    }
    Ok(shown.join(", "))
}

/// `{{ value | show(field) }}`: a value as its field in the metadata says
/// to show it. Money with the places of the company's currency; a unit
/// price with at least those and at most its scale; a quantity without
/// trailing zeros; another decimal with its scale; a reference by what it
/// refers to; a date as a date; a choice by its label.
fn show(
    state: &State,
    value: Value,
    field: Value,
    form: Option<String>,
) -> Result<String, TemplateError> {
    if value.is_undefined() || value.is_none() {
        return Ok(String::new());
    }
    let attr = |v: &Value, name: &str| v.get_attr(name).ok().filter(|v| !v.is_undefined());
    let ty = attr(&field, "type");
    let kind = ty
        .as_ref()
        .and_then(|ty| attr(ty, "kind"))
        .and_then(|k| k.as_str().map(str::to_owned))
        .unwrap_or_default();
    match kind.as_str() {
        "decimal" => {
            let Some(number) = to_decimal(&value)? else {
                return Ok(String::new());
            };
            let meta = attr(&field, "decimal");
            let role = meta
                .as_ref()
                .and_then(|m| attr(m, "role"))
                .and_then(|r| r.as_str().map(str::to_owned))
                .unwrap_or_default();
            let scale = meta
                .as_ref()
                .and_then(|m| attr(m, "scale"))
                .and_then(|s| u32::try_from(s).ok());
            let currency = currency_places(state);
            let places = match role.as_str() {
                "money" => Some(currency),
                "unit_price" => {
                    let needed = number.normalize().scale().max(currency);
                    Some(scale.map_or(needed, |scale| needed.min(scale)))
                }
                "quantity" => scale.map(|scale| number.normalize().scale().min(scale)),
                _ => scale,
            };
            Ok(format_decimal(number, places))
        }
        "link" | "document_link" | "line_link" | "entity_link" => {
            let shown = reference(state, value.clone(), form);
            Ok(if shown.is_empty() && kind != "link" {
                String::new()
            } else if shown.is_empty() {
                // A link by a key such as a code shows the code itself.
                let key = ty
                    .as_ref()
                    .and_then(|ty| attr(ty, "key"))
                    .and_then(|k| k.as_str().map(str::to_owned));
                if key.as_deref() == Some("id") {
                    String::new()
                } else {
                    value.as_str().unwrap_or_default().to_owned()
                }
            } else {
                shown
            })
        }
        "date" | "timestamp" => date(value, None),
        "bool" => Ok(if value.is_true() { "Yes" } else { "No" }.to_owned()),
        // A line's lots, from the stock ledger's `lots` field.
        "json"
            if attr(&field, "name")
                .and_then(|n| n.as_str().map(str::to_owned))
                .as_deref()
                == Some("lots") =>
        {
            lots(value)
        }
        "enum" => {
            let text = value.as_str().unwrap_or_default().to_owned();
            Ok(ty
                .as_ref()
                .and_then(|ty| attr(ty, "labels"))
                .and_then(|labels| attr(&labels, &text))
                .and_then(|label| label.as_str().map(str::to_owned))
                .unwrap_or(text))
        }
        _ => Ok(match value.as_str() {
            Some(text) => text.to_owned(),
            None => value.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decimals_format_exactly() {
        let d = |s: &str| Decimal::from_str(s).unwrap();
        assert_eq!(
            format_decimal(d("123456789012.34"), Some(2)),
            "123,456,789,012.34"
        );
        assert_eq!(
            format_decimal(d("123456789012.345"), Some(2)),
            "123,456,789,012.35"
        );
        assert_eq!(format_decimal(d("-1234.5"), Some(2)), "-1,234.50");
        assert_eq!(format_decimal(d("-0.001"), Some(2)), "0.00");
        assert_eq!(format_decimal(d("999"), Some(0)), "999");
        assert_eq!(format_decimal(d("1000"), None), "1,000");
        assert_eq!(format_decimal(d("12.5000"), None), "12.5");
        assert_eq!(format_decimal(d("0.1"), Some(3)), "0.100");
    }

    #[test]
    fn active_html_is_refused() {
        for refused in [
            "<script>alert(1)</script>",
            "<SCRIPT src=x></SCRIPT>",
            "<p onclick=\"x()\">",
            "<img src=x OnError=alert(1)>",
            "<a href=\"javascript:alert(1)\">",
            "<a href=\" JaVaScRiPt:alert(1)\">",
            "<a href=\"jav&#x61;script:alert(1)\">",
            "<a href=\"javascript&colon;alert(1)\">",
            "<iframe src=x>",
            "<object data=x>",
            "<embed src=x>",
            "<meta http-equiv=\"refresh\" content=\"0;url=x\">",
            "<base href=\"https://evil.example/\">",
            "<svg><a xlink:href=\"javascript:x\">",
            "<form action=\"data:text/html,x\">",
        ] {
            assert!(check_html(refused).is_err(), "{refused}");
        }
        for allowed in [
            "<p title=\"onclick=x javascript:y\">a &lt;script&gt; b</p>",
            "<table class=\"lines\"><td>1 < 2</td></table>",
            "<!-- <script> in a comment -->",
            "<meta charset=\"utf-8\">",
            "<a href=\"https://example.com/\">x</a>",
            "{% if x %}<b>{{ y }}</b>{% endif %}",
        ] {
            assert!(check_html(allowed).is_ok(), "{allowed}");
        }
    }

    /// What a template wrote, out of the page the server wraps it in.
    fn body(page: &str) -> String {
        let start = page.find("<body>\n").unwrap() + 7;
        let end = page.rfind("\n</body>").unwrap();
        page[start..end].to_owned()
    }

    #[test]
    fn templates_are_bounded_and_escaped() {
        let ctx = json!({ "memo": "<script>alert(1)</script>", "company": { "decimals": 2 } });
        let page = render("{{ memo }}", &ctx).unwrap();
        assert!(page.starts_with("<!doctype html>"));
        assert!(!page.contains("<script"), "{page}");
        assert_eq!(body(&page), "&lt;script&gt;alert(1)&lt;/script&gt;");
        // No way out of escaping.
        assert!(render("{{ memo | safe }}", &ctx).is_err());
        assert!(check_source("{% autoescape false %}{{ memo }}{% endautoescape %}").is_err());
        assert!(check_source("{%- autoescape false -%}{% endautoescape %}").is_err());
        // Nothing to include or import, and no way to name a file.
        assert!(check_source("{% include 'x.html' %}").is_err());
        assert!(check_source("{% extends 'x.html' %}").is_err());
        // Work that does not end, and output that does not stop.
        let started = std::time::Instant::now();
        let error = render(
            "{% for i in range(100000) %}{% for j in range(100000) %}{{ j }}{% endfor %}{% endfor %}",
            &ctx,
        )
        .unwrap_err();
        assert!(error.contains("too much work"), "{error}");
        let error = render(
            "{% set s = 'x' * 1000000 %}{% for i in range(100) %}{{ s }}{% endfor %}",
            &ctx,
        )
        .unwrap_err();
        assert!(
            error.contains("too much work") || error.contains("larger than"),
            "{error}"
        );
        assert!(started.elapsed().as_secs() < 10);
        // Data cannot make a link that runs code: the link loses it.
        let page = render(
            "<a href=\"{{ url }}\">x</a>",
            &json!({ "url": "javascript:alert(1)" }),
        )
        .unwrap();
        assert_eq!(body(&page), "<a rel=\"noopener noreferrer\">x</a>");
    }

    /// Every way past the old hand-written check that a review found, and a
    /// few more: none survives the sanitizer, whether the template wrote it
    /// or data did.
    #[test]
    fn the_sanitizer_keeps_nothing_that_runs() {
        let attacks = [
            // Raw text elements, where a parser sees no tags.
            "<textarea><p title=\"</textarea><img src=x onerror=alert(1)>\"></textarea>",
            "<title><p title=\"</title><img src=x onerror=alert(1)>\"></title>",
            "<style><p title=\"</style><img src=x onerror=alert(1)>\"></style>",
            // Bogus comments and the other comment end.
            "<?x><img src=x onerror=alert(1)>",
            "</ x><img src=x onerror=alert(1)>",
            "<!x><img src=x onerror=alert(1)>",
            "<!-- a --!><img src=x onerror=alert(1)>",
            // Character references a browser decodes without a semicolon,
            // padded, or with a tab inside the scheme.
            "<a href=\"&#106avascript:alert(1)\">x</a>",
            "<a href=\"&#0000106;avascript:alert(1)\">x</a>",
            "<a href=\"java&#9script:alert(1)\">x</a>",
            "<a href=\"java&Tab;script:alert(1)\">x</a>",
            // A URL not at the start of the attribute.
            "<svg><animate attributeName=\"href\" values=\"#;javascript:alert(1)\"/></svg>",
            "<math><a xlink:href=\"javascript:alert(1)\">x</a></math>",
            // Elements that load or run something.
            "<iframe src=\"https://evil.example\"></iframe>",
            "<object data=x></object><embed src=x>",
            "<form action=\"https://evil.example\"><button>x</button></form>",
            "<base href=\"https://evil.example/\">",
            "<meta http-equiv=\"refresh\" content=\"0;url=https://evil.example\">",
            "<link rel=stylesheet href=\"https://evil.example/x.css\">",
            "<p style=\"background:url(https://evil.example/x)\">x</p>",
            "<img src=\"data:image/svg+xml,<svg onload=alert(1)>\">",
        ];
        for attack in attacks {
            let clean = sanitize(attack).to_ascii_lowercase();
            for bad in [
                "onerror",
                "onload",
                "javascript",
                "<script",
                "<svg",
                "<math",
                "<iframe",
                "<object",
                "<embed",
                "<form",
                "<base",
                "<meta",
                "<link",
                "<style",
                "<textarea",
                "evil.example",
                "url(",
                "data:",
            ] {
                assert!(!clean.contains(bad), "{attack}\n -> {clean}");
            }
        }
        // What a printed page needs stays.
        let kept = sanitize(
            "<table class=\"lines\"><tr><td colspan=\"2\" style=\"text-align:right\">1</td></tr></table>\
             <a href=\"https://example.com/\">x</a><a href=\"#totals\">y</a><img src=\"https://example.com/logo.png\" alt=\"logo\">",
        );
        for wanted in [
            "<table class=\"lines\">",
            "colspan=\"2\"",
            "style=\"text-align:right\"",
            "href=\"https://example.com/\"",
            "href=\"#totals\"",
            "src=\"https://example.com/logo.png\"",
        ] {
            assert!(kept.contains(wanted), "{wanted}: {kept}");
        }
    }

    /// Escaping can be undone in a template: a `<` kept aside and printed
    /// before data makes the data a tag. The sanitizer then takes it apart.
    #[test]
    fn markup_made_from_data_is_sanitized() {
        let ctx = json!({ "memo": "img src=x onerror=alert(1)>" });
        let page = render("{% set lt %}<{% endset %}{{ lt }}{{ memo }}", &ctx).unwrap();
        assert!(!page.contains("onerror"), "{page}");
        let page = render(
            "{% set lt %}<{% endset %}{{ memo | replace('img', lt ~ 'img') }}",
            &json!({ "memo": "img src=x onerror=alert(1)>" }),
        )
        .unwrap();
        assert!(!page.contains("onerror"), "{page}");
    }

    #[test]
    fn filters_format_money_numbers_and_dates() {
        let ctx = json!({
            "company": { "decimals": 2 },
            "amount": "123456789012.3400",
            "qty": "10.500000",
            "day": "2026-03-31",
            "refs": { "abc": { "display": "W-1 Widget", "short": "W-1" } },
        });
        let page = body(&render(
            "{{ '3' | mul('0.1') }}|{{ '12.5' | price }}|{{ '1.23456' | price }}|{{ amount | money }}|{{ amount | money(0) }}|{{ qty | decimal }}|{{ qty | decimal(2) }}|{{ day | date }}|{{ day | date('%d.%m.%Y') }}|{{ 'abc' | ref }}|{{ 'abc' | ref('short') }}|{{ 'nope' | ref }}|{{ missing | money }}",
            &ctx,
        )
        .unwrap());
        assert_eq!(
            page,
            "0.3|12.50|1.23456|123,456,789,012.34|123,456,789,012|10.5|10.50|31 Mar 2026|31.03.2026|W-1 Widget|W-1||"
        );
        assert!(render("{{ day | date('%Q') }}", &ctx).is_err());
    }
}
