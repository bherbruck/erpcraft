//! The XML embedded in a ZUGFeRD / Factur-X PDF.
//!
//! A PDF is hostile input: it is parsed with `lopdf` (pure Rust), never
//! rendered and never executed. Before anything is read, the file must be
//! at most [`MAX_PDF_BYTES`]; a PDF that carries JavaScript is refused;
//! the embedded file is found among the file specifications by the names
//! ZUGFeRD and Factur-X give it; and it is inflated by this module with a
//! limit, so that a small stream cannot inflate into gigabytes.
//!
//! What `lopdf` itself inflates while loading (object streams and
//! cross-reference streams) is bounded per stream by
//! [`MAX_STREAM_BYTES`], and what a load keeps is bounded by a budget of
//! objects and bytes ([`MAX_OBJECTS`], [`MAX_KEPT_BYTES`]): a file that
//! goes beyond is refused. Parsing runs on a thread of its own with a
//! large stack, waited for from Tokio's blocking pool with a time limit;
//! a panic in the parser is a refusal, not a crash.

use std::cell::Cell;
use std::io::Read;
use std::time::Duration;

use lopdf::{Document, LoadOptions, Object};

pub const MAX_PDF_BYTES: usize = 20 * 1024 * 1024;
/// The most one stream may inflate to while the PDF is loaded.
pub const MAX_STREAM_BYTES: usize = 4 * 1024 * 1024;
/// The most objects a PDF may hold.
pub const MAX_OBJECTS: usize = 200_000;
/// The most bytes the loaded objects may hold together.
pub const MAX_KEPT_BYTES: usize = 64 * 1024 * 1024;
/// How long reading a PDF may take.
pub const TIME_LIMIT: Duration = Duration::from_secs(20);

thread_local! {
    /// The objects and bytes kept by the load running on this thread, and
    /// whether it went beyond the budget.
    static BUDGET: Cell<(usize, usize, bool)> = const { Cell::new((0, 0, false)) };
}

/// About how many bytes an object holds, nested objects included, to a
/// depth that a sane PDF does not exceed.
fn weight(object: &Object, depth: usize) -> usize {
    if depth > 32 {
        return 16;
    }
    16 + match object {
        Object::String(bytes, _) | Object::Name(bytes) => bytes.len(),
        Object::Array(items) => items.iter().map(|o| weight(o, depth + 1)).sum(),
        Object::Dictionary(d) => d.iter().map(|(k, v)| k.len() + weight(v, depth + 1)).sum(),
        Object::Stream(s) => {
            s.content.len()
                + s.dict
                    .iter()
                    .map(|(k, v)| k.len() + weight(v, depth + 1))
                    .sum::<usize>()
        }
        _ => 0,
    }
}

/// The load filter: keeps objects while the budget lasts.
fn budget(id: (u32, u16), object: &mut Object) -> Option<((u32, u16), Object)> {
    let size = weight(object, 0);
    let (objects, bytes, over) = BUDGET.get();
    let (objects, bytes) = (objects + 1, bytes.saturating_add(size));
    let over = over || objects > MAX_OBJECTS || bytes > MAX_KEPT_BYTES;
    BUDGET.set((objects, bytes, over));
    if over {
        return None;
    }
    // lopdf keeps the object it passed for a top-level object and the one
    // returned for an object of an object stream: both must be it.
    Some((id, object.clone()))
}

/// The names an invoice's XML has in a hybrid PDF: Factur-X and ZUGFeRD
/// 2.1 on, ZUGFeRD 2.0, ZUGFeRD 1, and XRechnung in a ZUGFeRD PDF.
const NAMES: &[&str] = &[
    "factur-x.xml",
    "zugferd-invoice.xml",
    "ZUGFeRD-invoice.xml",
    "xrechnung.xml",
];

/// Whether the content is a PDF.
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-")
}

/// The XML a hybrid invoice PDF carries.
pub async fn embedded_xml(bytes: Vec<u8>) -> Result<String, String> {
    if bytes.len() > MAX_PDF_BYTES {
        return Err(format!(
            "the PDF is larger than {} MiB",
            MAX_PDF_BYTES / 1024 / 1024
        ));
    }
    let worker = std::thread::Builder::new()
        .name("einvoice-pdf".into())
        .stack_size(64 * 1024 * 1024)
        .spawn(move || std::panic::catch_unwind(|| extract(&bytes)))
        .map_err(|e| format!("the PDF could not be read: {e}"))?;
    // Waited for on the blocking pool, never on a runtime worker.
    let waited = tokio::time::timeout(
        TIME_LIMIT,
        tokio::task::spawn_blocking(move || worker.join()),
    )
    .await;
    match waited {
        Ok(Ok(Ok(Ok(result)))) => result,
        Err(_) => Err("the PDF could not be read in time".into()),
        _ => Err("the PDF could not be read: it is malformed".into()),
    }
}

fn name_of(object: &Object) -> Option<String> {
    match object {
        Object::String(bytes, _) => {
            // A PDF text string is PDFDocEncoding or UTF-16 with a BOM.
            if bytes.starts_with(&[0xFE, 0xFF]) {
                let units: Vec<u16> = bytes[2..]
                    .chunks(2)
                    .filter(|c| c.len() == 2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                Some(String::from_utf16_lossy(&units))
            } else {
                Some(String::from_utf8_lossy(bytes).into_owned())
            }
        }
        _ => None,
    }
}

fn extract(bytes: &[u8]) -> Result<String, String> {
    BUDGET.set((0, 0, false));
    let options = LoadOptions {
        max_decompressed_size: Some(MAX_STREAM_BYTES),
        filter: Some(budget),
        ..LoadOptions::default()
    };
    let doc = Document::load_mem_with_options(bytes, options)
        .map_err(|e| format!("the PDF could not be read: {e}"))?;
    if BUDGET.get().2 {
        return Err(format!(
            "the PDF holds more than {MAX_OBJECTS} objects or {} MiB once loaded",
            MAX_KEPT_BYTES / 1024 / 1024
        ));
    }
    // Nothing that runs: a JavaScript action anywhere refuses the file.
    for object in doc.objects.values() {
        let dict = match object {
            Object::Dictionary(d) => d,
            Object::Stream(s) => &s.dict,
            _ => continue,
        };
        let javascript = dict.has(b"JS")
            || matches!(dict.get(b"S"), Ok(Object::Name(n)) if n.as_slice() == b"JavaScript")
            || dict.has(b"JavaScript");
        if javascript {
            return Err("the PDF contains JavaScript and is not accepted".into());
        }
    }
    for object in doc.objects.values() {
        let Object::Dictionary(spec) = object else {
            continue;
        };
        let Ok(Object::Dictionary(ef)) = spec
            .get(b"EF")
            .and_then(|ef| doc.dereference(ef).map(|(_, o)| o))
        else {
            continue;
        };
        let name = spec
            .get(b"UF")
            .ok()
            .and_then(name_of)
            .or_else(|| spec.get(b"F").ok().and_then(name_of))
            .unwrap_or_default();
        if !NAMES.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
            continue;
        }
        let Ok(file) = ef.get(b"F").or_else(|_| ef.get(b"UF")) else {
            continue;
        };
        let Ok((_, Object::Stream(stream))) = doc.dereference(file) else {
            continue;
        };
        let filter = stream.dict.get(b"Filter").ok().map(|f| match f {
            Object::Name(n) => vec![n.clone()],
            Object::Array(a) => a
                .iter()
                .filter_map(|o| match o {
                    Object::Name(n) => Some(n.clone()),
                    _ => None,
                })
                .collect(),
            _ => vec![b"?".to_vec()],
        });
        let content = match filter.as_deref() {
            None | Some([]) => stream.content.clone(),
            Some([f]) if f.as_slice() == b"FlateDecode" => inflate(&stream.content)?,
            Some(_) => {
                return Err(format!(
                    "the embedded {name} is compressed in a way that is not read here"
                ));
            }
        };
        return String::from_utf8(content)
            .map_err(|_| format!("the embedded {name} is not UTF-8 text"));
    }
    Err(
        "the PDF has no embedded invoice (factur-x.xml, zugferd-invoice.xml or xrechnung.xml); \
         a PDF without one is not an electronic invoice"
            .into(),
    )
}

/// Inflate a stream, refusing anything that inflates beyond the XML limit.
fn inflate(data: &[u8]) -> Result<Vec<u8>, String> {
    let limit = crate::xml::MAX_BYTES as u64 + 1;
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(data)
        .take(limit)
        .read_to_end(&mut out)
        .map_err(|e| format!("the embedded invoice could not be inflated: {e}"))?;
    if out.len() as u64 >= limit {
        return Err("the embedded invoice is larger than the XML limit".into());
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal PDF with `xml` embedded as `name`, compressed or not,
    /// optionally with a JavaScript action.
    pub fn build(xml: &str, name: &str, compress: bool, javascript: bool) -> Vec<u8> {
        use lopdf::{Dictionary, Stream, dictionary};
        let mut doc = Document::with_version("1.7");
        let content = if compress {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut e, xml.as_bytes()).unwrap();
            e.finish().unwrap()
        } else {
            xml.as_bytes().to_vec()
        };
        let mut stream_dict = dictionary! { "Type" => "EmbeddedFile", "Subtype" => "text/xml" };
        if compress {
            stream_dict.set("Filter", "FlateDecode");
        }
        let file = doc.add_object(Stream::new(stream_dict, content));
        let spec = doc.add_object(dictionary! {
            "Type" => "Filespec",
            "F" => Object::string_literal(name),
            "UF" => Object::string_literal(name),
            "AFRelationship" => "Alternative",
            "EF" => dictionary! { "F" => file },
        });
        let names = doc.add_object(dictionary! {
            "Names" => vec![Object::string_literal(name), spec.into()],
        });
        let pages =
            doc.add_object(dictionary! { "Type" => "Pages", "Kids" => vec![], "Count" => 0 });
        let mut catalog: Dictionary = dictionary! {
            "Type" => "Catalog",
            "Pages" => pages,
            "Names" => dictionary! { "EmbeddedFiles" => names },
            "AF" => vec![spec.into()],
        };
        if javascript {
            let js = doc.add_object(dictionary! {
                "S" => "JavaScript",
                "JS" => Object::string_literal("app.alert('x')"),
            });
            catalog.set("OpenAction", js);
        }
        let catalog = doc.add_object(catalog);
        doc.trailer.set("Root", catalog);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    #[tokio::test]
    async fn finds_the_embedded_invoice_and_refuses_javascript() {
        let xml = "<rsm:CrossIndustryInvoice xmlns:rsm=\"x\"/>";
        for compress in [false, true] {
            let pdf = build(xml, "factur-x.xml", compress, false);
            assert!(is_pdf(&pdf));
            assert_eq!(embedded_xml(pdf).await.unwrap(), xml);
        }
        let pdf = build(xml, "factur-x.xml", true, true);
        assert!(embedded_xml(pdf).await.unwrap_err().contains("JavaScript"));
        let pdf = build(xml, "notes.xml", false, false);
        assert!(
            embedded_xml(pdf)
                .await
                .unwrap_err()
                .contains("no embedded invoice")
        );
        assert!(embedded_xml(b"%PDF-1.7 garbage".to_vec()).await.is_err());
    }

    #[tokio::test]
    async fn a_stream_that_inflates_too_far_is_refused() {
        let big = "a".repeat(crate::xml::MAX_BYTES + 10);
        let pdf = build(&big, "factur-x.xml", true, false);
        assert!(embedded_xml(pdf).await.unwrap_err().contains("larger"));
    }

    /// Written by hand: a PDF whose object stream of a few kilobytes
    /// inflates to `inflated` bytes.
    fn object_stream_pdf(inflated: usize) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut e, b"4 0 ").unwrap();
        std::io::Write::write_all(&mut e, &vec![b' '; inflated - 4]).unwrap();
        let bomb = e.finish().unwrap();
        assert!(bomb.len() < 64 * 1024, "a small file");
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = vec![];
        for body in [
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec(),
            [
                format!(
                    "<< /Type /ObjStm /N 1 /First 4 /Filter /FlateDecode /Length {} >>\nstream\n",
                    bomb.len()
                )
                .into_bytes(),
                bomb.clone(),
                b"\nendstream".to_vec(),
            ]
            .concat(),
        ] {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n", offsets.len()).as_bytes());
            out.extend(body);
            out.extend(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend(b"xref\n0 4\n0000000000 65535 f \n");
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        out
    }

    #[tokio::test]
    async fn an_object_stream_bomb_is_refused_while_loading() {
        // Within the limit, the file loads (and has no invoice in it).
        let error = embedded_xml(object_stream_pdf(MAX_STREAM_BYTES - 1024))
            .await
            .unwrap_err();
        assert!(error.contains("no embedded invoice"), "{error}");
        // Beyond, loading stops at that stream.
        let error = embedded_xml(object_stream_pdf(MAX_STREAM_BYTES + 1024))
            .await
            .unwrap_err();
        assert!(error.contains("could not be read"), "{error}");
    }

    #[test]
    fn the_budget_stops_a_load_that_keeps_too_much() {
        BUDGET.set((0, 0, false));
        let mut big = Object::String(vec![b'x'; MAX_KEPT_BYTES / 2], lopdf::StringFormat::Literal);
        assert!(budget((1, 0), &mut big.clone()).is_some());
        assert!(budget((2, 0), &mut big).is_none());
        assert!(BUDGET.get().2);
    }
}
