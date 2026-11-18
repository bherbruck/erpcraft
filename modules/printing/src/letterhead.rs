//! The company's letterhead: what every printed document says about who
//! sends it. The legal name, address, tax identifiers and register entry
//! come from party data, the account it is paid into from banking, and the
//! logo from an image uploaded here. Nothing is fetched from anywhere.
//!
//! A template prints the logo with `<img class="logo" src="#logo">`; the
//! renderer puts the image in after the page is sanitised, as a `data:`
//! URL of a PNG or JPEG whose bytes it checked, so no template can bring
//! in a URL of its own.

use base64::Engine;
use erpcraft_kernel::{
    Command, CommandSpec, Ctx, Error, JsonSchema, PgConnection, Result, async_trait, schemars,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// The largest logo kept, in bytes.
pub(crate) const MAX_LOGO: usize = 512 * 1024;

/// The logo's bytes and type, when the company has one.
pub(crate) async fn logo(
    conn: &mut PgConnection,
    company_id: Uuid,
) -> Result<Option<(String, Vec<u8>)>> {
    Ok(
        sqlx::query_as("SELECT content_type, data FROM company_logo WHERE company_id = $1")
            .bind(company_id)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

/// What type of image `data` is, by its first bytes: PNG or JPEG.
fn image_type(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else {
        None
    }
}

/// Put the company's logo where a sanitised page asks for it.
pub(crate) fn place_logo(html: &str, logo: Option<&(String, Vec<u8>)>) -> String {
    let Some((content_type, data)) = logo else {
        return html.to_owned();
    };
    let url = format!(
        "data:{content_type};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(data)
    );
    html.replace("src=\"#logo\"", &format!("src=\"{url}\""))
}

/// The letterhead a template sees as `letterhead`: the company's
/// particulars as far as the caller may read the company, else its name.
pub(crate) async fn letterhead(ctx: &mut Ctx<'_>, company_id: Uuid, name: &str) -> Result<Value> {
    let has_logo: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM company_logo WHERE company_id = $1)")
            .bind(company_id)
            .fetch_one(&mut *ctx.conn)
            .await?;
    let Some(party) = erpcraft_party::load_readable(ctx, "company", company_id).await? else {
        return Ok(json!({ "name": name, "logo": has_logo }));
    };
    let bank = match party.payment_account_id {
        Some(account) => erpcraft_banking::account_number(&mut *ctx.conn, account).await?,
        None => None,
    };
    let address = party.address.clone();
    Ok(json!({
        "name": party.legal_name.clone().unwrap_or_else(|| name.to_owned()),
        "trading_name": party.trading_name,
        "street": address.as_ref().and_then(|a| a.street.clone()),
        "additional_street": address.as_ref().and_then(|a| a.additional_street.clone()),
        "postcode": address.as_ref().and_then(|a| a.postcode.clone()),
        "city": address.as_ref().and_then(|a| a.city.clone()),
        "region": address.as_ref().and_then(|a| a.region.clone()),
        "country_code": address.as_ref().map(|a| a.country_code.clone()),
        "vat_id": party.tax_id("eu_vat"),
        "tax_number": party.tax_id("tax_number"),
        "us_ein": party.tax_id("us_ein"),
        "ca_bn": party.tax_id("ca_bn"),
        "legal_registration": party.legal_registration,
        "legal_info": party.legal_info,
        "phone": party.contact_phone,
        "email": party.contact_email,
        "bank": bank.map(|b| json!({
            "iban": b.iban, "bic": b.bic, "account_number": b.account_number,
            "account_name": b.account_name,
        })),
        "logo": has_logo,
    }))
}

// ---------------------------------------------------------------- commands

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetInput {
    /// The company.
    company_id: Uuid,
    /// The image, a PNG or JPEG of at most 512 KiB, in base64.
    data: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompanyInput {
    /// The company.
    company_id: Uuid,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct LogoOutput {
    company_id: Uuid,
    /// `image/png` or `image/jpeg`; absent when the company has no logo.
    content_type: Option<String>,
    /// The image in base64; absent when the company has no logo.
    data: Option<String>,
    /// Its size in bytes.
    size: Option<usize>,
}

fn output(company_id: Uuid, logo: Option<(String, Vec<u8>)>) -> LogoOutput {
    match logo {
        Some((content_type, data)) => LogoOutput {
            company_id,
            content_type: Some(content_type),
            size: Some(data.len()),
            data: Some(base64::engine::general_purpose::STANDARD.encode(data)),
        },
        None => LogoOutput {
            company_id,
            content_type: None,
            data: None,
            size: None,
        },
    }
}

pub(crate) struct SetLogo;

#[async_trait]
impl Command for SetLogo {
    type Input = SetInput;
    type Output = LogoOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "company_logo.set",
            "Upload the company's logo, printed on its letterhead: a PNG or JPEG of at most \
             512 KiB, in base64. It replaces the one before. Takes the right to change the \
             company.",
        )
        .label("Set logo")
        .resource("company")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: SetInput) -> Result<LogoOutput> {
        ctx.require("company:update", input.company_id)?;
        let data = base64::engine::general_purpose::STANDARD
            .decode(input.data.trim())
            .map_err(|_| Error::field("data", "the image is not valid base64"))?;
        if data.is_empty() || data.len() > MAX_LOGO {
            return Err(Error::field("data", "a logo is at most 512 KiB"));
        }
        let content_type = image_type(&data)
            .ok_or_else(|| Error::field("data", "a logo is a PNG or a JPEG image"))?;
        sqlx::query(
            "INSERT INTO company_logo (company_id, content_type, data) VALUES ($1, $2, $3)
             ON CONFLICT (company_id) DO UPDATE SET content_type = EXCLUDED.content_type,
                 data = EXCLUDED.data, updated_at = now()",
        )
        .bind(input.company_id)
        .bind(content_type)
        .bind(&data)
        .execute(&mut *ctx.conn)
        .await?;
        ctx.touch("company", input.company_id);
        Ok(output(input.company_id, Some((content_type.into(), data))))
    }
}

pub(crate) struct GetLogo;

#[async_trait]
impl Command for GetLogo {
    type Input = CompanyInput;
    type Output = LogoOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::query(
            "company_logo.get",
            "The company's logo, in base64, if it has one. Takes the right to read the company.",
        )
        .label("Logo")
        .resource("company")
        .target(false)
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: CompanyInput) -> Result<LogoOutput> {
        ctx.require("company:read", input.company_id)?;
        let found = logo(&mut *ctx.conn, input.company_id).await?;
        Ok(output(input.company_id, found))
    }
}

pub(crate) struct RemoveLogo;

#[async_trait]
impl Command for RemoveLogo {
    type Input = CompanyInput;
    type Output = LogoOutput;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action(
            "company_logo.remove",
            "Remove the company's logo from its letterhead. Takes the right to change the \
             company.",
        )
        .label("Remove logo")
        .resource("company")
        .target(false)
        .destructive()
    }

    async fn run(&self, ctx: &mut Ctx<'_>, input: CompanyInput) -> Result<LogoOutput> {
        ctx.require("company:update", input.company_id)?;
        let removed = sqlx::query("DELETE FROM company_logo WHERE company_id = $1")
            .bind(input.company_id)
            .execute(&mut *ctx.conn)
            .await?
            .rows_affected();
        if removed == 0 {
            ctx.no_change();
        } else {
            ctx.touch("company", input.company_id);
        }
        Ok(output(input.company_id, None))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_png_and_jpeg_are_logos() {
        assert_eq!(image_type(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(image_type(b"\xFF\xD8\xFF\xE0..."), Some("image/jpeg"));
        assert_eq!(image_type(b"<svg onload=alert(1)>"), None);
        assert_eq!(image_type(b"GIF89a"), None);
    }

    #[test]
    fn the_logo_goes_only_where_the_page_asks() {
        let logo = ("image/png".to_owned(), vec![1, 2, 3]);
        let html = "<img class=\"logo\" src=\"#logo\" alt=\"\"><a href=\"#logo\">x</a>";
        let placed = place_logo(html, Some(&logo));
        assert!(placed.contains("src=\"data:image/png;base64,AQID\""));
        assert!(placed.contains("href=\"#logo\""));
        assert_eq!(place_logo(html, None), html);
    }
}
