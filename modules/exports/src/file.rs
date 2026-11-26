//! The files the exports make: how they are written, and the record that
//! one was made.

use base64::Engine;
use chrono::NaiveDate;
use erpcraft_kernel::{Ctx, Decimal, JsonSchema, Result, new_id, schemars};
use serde::Serialize;
use sha2::Digest;
use uuid::Uuid;

/// A file an export made.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct ExportFile {
    /// What to save it as.
    pub filename: String,
    /// The media type, with its character set.
    pub content_type: String,
    /// The file's bytes, in base64.
    pub data: String,
    /// The file as text, for reading.
    pub text: String,
    /// The SHA-256 of the bytes, in hex: what the export log keeps.
    pub sha256: String,
    /// The rows of data in it.
    pub rows: usize,
    /// What the file leaves out or could not say, for whoever sends it on.
    pub warnings: Vec<String>,
}

impl ExportFile {
    pub(crate) fn new(
        filename: String,
        content_type: &str,
        text: String,
        bytes: Vec<u8>,
        rows: usize,
        warnings: Vec<String>,
    ) -> ExportFile {
        let sha256 = sha2::Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        ExportFile {
            filename,
            content_type: content_type.into(),
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            text,
            sha256,
            rows,
            warnings,
        }
    }

    /// Record that the file was made. The log is never changed.
    pub(crate) async fn log(
        &self,
        ctx: &mut Ctx<'_>,
        company_id: Uuid,
        kind: &str,
        range: Option<(NaiveDate, NaiveDate)>,
        fixed: bool,
    ) -> Result<()> {
        let id = new_id();
        sqlx::query(
            "INSERT INTO export_log (id, company_id, kind, from_date, to_date, rows, filename,
                 sha256, fixed, created_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(id)
        .bind(company_id)
        .bind(kind)
        .bind(range.map(|r| r.0))
        .bind(range.map(|r| r.1))
        .bind(self.rows as i32)
        .bind(&self.filename)
        .bind(&self.sha256)
        .bind(fixed)
        .bind(ctx.actor.user_id)
        .execute(&mut *ctx.conn)
        .await?;
        ctx.touch("export_log", id);
        Ok(())
    }
}

/// `text` in Windows-1252, as DATEV's files are: a character it does not
/// have is written `?`.
pub(crate) fn windows_1252(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| match c as u32 {
            0x00..=0x7F | 0xA0..=0xFF => c as u32 as u8,
            _ => match c {
                '€' => 0x80,
                '‚' => 0x82,
                'ƒ' => 0x83,
                '„' => 0x84,
                '…' => 0x85,
                '†' => 0x86,
                '‡' => 0x87,
                'ˆ' => 0x88,
                '‰' => 0x89,
                'Š' => 0x8A,
                '‹' => 0x8B,
                'Œ' => 0x8C,
                'Ž' => 0x8E,
                '‘' => 0x91,
                '’' => 0x92,
                '“' => 0x93,
                '”' => 0x94,
                '•' => 0x95,
                '–' => 0x96,
                '—' => 0x97,
                '˜' => 0x98,
                '™' => 0x99,
                'š' => 0x9A,
                '›' => 0x9B,
                'œ' => 0x9C,
                'ž' => 0x9E,
                'Ÿ' => 0x9F,
                _ => b'?',
            },
        })
        .collect()
}

/// A text field as DATEV writes it: in double quotes, a quote doubled, no
/// line break, at most `max` characters.
pub(crate) fn datev_text(text: &str, max: usize) -> String {
    let clean: String = text
        .chars()
        .map(|c| {
            if c == '\r' || c == '\n' || c == ';' {
                ' '
            } else {
                c
            }
        })
        .take(max)
        .collect();
    format!("\"{}\"", clean.replace('"', "\"\""))
}

/// An amount as DATEV writes it: two decimals, a comma.
pub(crate) fn datev_amount(amount: Decimal) -> String {
    format!("{:.2}", amount.round_dp(2)).replace('.', ",")
}

/// A field of a comma-separated file: quoted when it has to be.
pub(crate) fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) || text.starts_with(' ') || text.ends_with(' ') {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datev_fields_are_written_as_datev_reads_them() {
        assert_eq!(
            windows_1252("Größe € 5 – ✓"),
            b"Gr\xf6\xdfe \x80 5 \x96 ?".to_vec()
        );
        assert_eq!(
            datev_text("Fachbuch \"Controlling\"; Teil 1\n", 60),
            "\"Fachbuch \"\"Controlling\"\"  Teil 1 \""
        );
        assert_eq!(datev_text("abcdef", 3), "\"abc\"");
        assert_eq!(datev_amount(Decimal::new(249500, 4)), "24,95");
        assert_eq!(datev_amount(Decimal::new(1234567, 0)), "1234567,00");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("plain"), "plain");
    }
}
