//! An XMP extension schema added to a PDF/A that `krilla` wrote: what
//! Factur-X / ZUGFeRD asks of the PDF that carries an invoice (the `fx`
//! schema, declared in the PDF/A extension schemas, with the document's
//! type, file name, version and conformance level).
//!
//! krilla writes the XMP packet as an unfiltered stream and the file with
//! a classic cross-reference table. The packet is rewritten in place and
//! every offset after it moved by what the packet grew, so the file stays
//! one revision, as PDF/A validators like best.

/// A schema of XMP properties to add, and the values to give them.
#[derive(Debug, Clone)]
pub struct XmpExtension {
    /// The schema's name, as PDF/A's extension schema list shows it.
    pub schema: String,
    pub namespace: String,
    pub prefix: String,
    pub properties: Vec<XmpProperty>,
}

#[derive(Debug, Clone)]
pub struct XmpProperty {
    pub name: String,
    pub value: String,
    /// `Text`, the XMP value type.
    pub value_type: String,
    /// `external` or `internal`.
    pub category: String,
    pub description: String,
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

fn rfind(haystack: &[u8], needle: &[u8], before: usize) -> Option<usize> {
    haystack[..before]
        .windows(needle.len())
        .rposition(|w| w == needle)
}

/// The packet with the extension's schema declared and its properties set.
fn extend(xmp: &str, ext: &XmpExtension) -> Result<String, String> {
    let schema = format!(
        "<rdf:li rdf:parseType=\"Resource\"><pdfaSchema:schema>{}</pdfaSchema:schema>\
         <pdfaSchema:namespaceURI>{}</pdfaSchema:namespaceURI>\
         <pdfaSchema:prefix>{}</pdfaSchema:prefix><pdfaSchema:property><rdf:Seq>{}</rdf:Seq>\
         </pdfaSchema:property></rdf:li>",
        escape(&ext.schema),
        escape(&ext.namespace),
        escape(&ext.prefix),
        ext.properties
            .iter()
            .map(|p| format!(
                "<rdf:li rdf:parseType=\"Resource\"><pdfaProperty:name>{}</pdfaProperty:name>\
                 <pdfaProperty:valueType>{}</pdfaProperty:valueType>\
                 <pdfaProperty:category>{}</pdfaProperty:category>\
                 <pdfaProperty:description>{}</pdfaProperty:description></rdf:li>",
                escape(&p.name),
                escape(&p.value_type),
                escape(&p.category),
                escape(&p.description)
            ))
            .collect::<String>()
    );
    let values: String = ext
        .properties
        .iter()
        .map(|p| {
            format!(
                "<{0}:{1}>{2}</{0}:{1}>",
                ext.prefix,
                p.name,
                escape(&p.value)
            )
        })
        .collect();
    let mut out = xmp.to_owned();
    // The schema goes into the list of extension schemas, or a list of its
    // own when there is none.
    const BAG: &str = "<pdfaExtension:schemas><rdf:Bag>";
    let description = out
        .find("<rdf:Description rdf:about=\"\"")
        .ok_or("the XMP has no description")?;
    let close = out[description..]
        .find("</rdf:Description>")
        .map(|i| i + description)
        .ok_or("the XMP description is not closed")?;
    let schemas = match out.find(BAG) {
        Some(at) => {
            out.insert_str(at + BAG.len(), &schema);
            String::new()
        }
        None => format!("{BAG}{schema}</rdf:Bag></pdfaExtension:schemas>"),
    };
    let close = if schemas.is_empty() {
        close + schema.len()
    } else {
        close
    };
    out.insert_str(close, &format!("{values}{schemas}"));
    // The namespaces the properties use, on the description.
    let mut namespaces = format!(" xmlns:{}=\"{}\"", ext.prefix, escape(&ext.namespace));
    if !schemas.is_empty() && !out.contains("xmlns:pdfaExtension=") {
        namespaces.push_str(
            " xmlns:pdfaExtension=\"http://www.aiim.org/pdfa/ns/extension/\" \
             xmlns:pdfaSchema=\"http://www.aiim.org/pdfa/ns/schema#\" \
             xmlns:pdfaProperty=\"http://www.aiim.org/pdfa/ns/property#\"",
        );
    }
    let at = description + "<rdf:Description rdf:about=\"\"".len();
    out.insert_str(at, &namespaces);
    Ok(out)
}

/// `pdf` with `ext` added to its XMP metadata.
pub fn add_extension(pdf: &[u8], ext: &XmpExtension) -> Result<Vec<u8>, String> {
    let start = find(pdf, b"<x:xmpmeta", 0).ok_or("the PDF has no XMP metadata")?;
    let end = find(pdf, b"</x:xmpmeta>", start)
        .map(|i| i + b"</x:xmpmeta>".len())
        .ok_or("the XMP metadata is not closed")?;
    let old = std::str::from_utf8(&pdf[start..end]).map_err(|_| "the XMP is not UTF-8")?;
    let new = extend(old, ext)?;
    // The stream's dictionary, with its length.
    let object = rfind(pdf, b" obj", start).ok_or("the XMP is not in an object")?;
    let length_at = find(&pdf[..start], b"/Length ", object)
        .ok_or("the XMP stream states no length")?
        + b"/Length ".len();
    let length_end = length_at
        + pdf[length_at..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .ok_or("the XMP stream's length is not closed")?;
    let old_length: usize = std::str::from_utf8(&pdf[length_at..length_end])
        .ok()
        .and_then(|t| t.parse().ok())
        .ok_or("the XMP stream's length is not a number")?;
    // A packet with more than the xmpmeta element (an xpacket wrapper)
    // keeps its other bytes; only the element's size changes.
    let new_length = old_length + new.len() - old.len();
    let length_text = new_length.to_string();
    let grow_length = length_text.len() as isize - (length_end - length_at) as isize;
    let grow_packet = new.len() as isize - old.len() as isize;
    let shift = |offset: usize| -> usize {
        let mut o = offset as isize;
        if offset > length_at {
            o += grow_length;
        }
        if offset > start {
            o += grow_packet;
        }
        o as usize
    };
    let mut out = Vec::with_capacity(pdf.len() + new.len());
    out.extend_from_slice(&pdf[..length_at]);
    out.extend_from_slice(length_text.as_bytes());
    out.extend_from_slice(&pdf[length_end..start]);
    out.extend_from_slice(new.as_bytes());
    out.extend_from_slice(&pdf[end..]);
    // The cross-reference table and where it starts.
    let startxref = rfind(&out, b"startxref", out.len()).ok_or("the PDF has no startxref")?;
    let digits_at = startxref + b"startxref".len();
    let digits: Vec<(usize, usize)> = {
        let s = digits_at
            + out[digits_at..]
                .iter()
                .position(u8::is_ascii_digit)
                .ok_or("startxref has no offset")?;
        let e = s + out[s..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .unwrap_or(0);
        vec![(s, e)]
    };
    let (s, e) = digits[0];
    let xref_old: usize = std::str::from_utf8(&out[s..e])
        .ok()
        .and_then(|t| t.parse().ok())
        .ok_or("startxref is not a number")?;
    let xref_new = shift(xref_old);
    if out.get(xref_new..xref_new + 4) != Some(b"xref") {
        return Err("the PDF has no classic cross-reference table where it says".into());
    }
    // The entries: "oooooooooo ggggg n\r\n", 20 bytes each.
    let header_end = find(&out, b"\n", xref_new).ok_or("bad xref")? + 1;
    let subsection_end = find(&out, b"\n", header_end).ok_or("bad xref")? + 1;
    let header = std::str::from_utf8(&out[header_end..subsection_end]).map_err(|_| "bad xref")?;
    let mut parts = header.split_whitespace();
    let _first: usize = parts
        .next()
        .and_then(|t| t.parse().ok())
        .ok_or("bad xref subsection")?;
    let count: usize = parts
        .next()
        .and_then(|t| t.parse().ok())
        .ok_or("bad xref subsection")?;
    for i in 0..count {
        let entry = subsection_end + i * 20;
        if out.get(entry + 17) != Some(&b'n') {
            continue;
        }
        let offset: usize = std::str::from_utf8(&out[entry..entry + 10])
            .ok()
            .and_then(|t| t.parse().ok())
            .ok_or("bad xref entry")?;
        let moved = format!("{:010}", shift(offset));
        out[entry..entry + 10].copy_from_slice(moved.as_bytes());
    }
    let tail = format!("{xref_new}");
    let mut fixed = out[..s].to_vec();
    fixed.extend_from_slice(tail.as_bytes());
    fixed.extend_from_slice(&out[e..]);
    Ok(fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_is_declared_and_its_values_set() {
        let xmp = "<x:xmpmeta><rdf:RDF><rdf:Description rdf:about=\"\" xmlns:a=\"b\">\
                   <pdfaExtension:schemas><rdf:Bag><rdf:li>x</rdf:li></rdf:Bag>\
                   </pdfaExtension:schemas></rdf:Description></rdf:RDF></x:xmpmeta>";
        let ext = XmpExtension {
            schema: "Factur-X PDFA Extension Schema".into(),
            namespace: "urn:factur-x:pdfa:CrossIndustryDocument:invoice:1p0#".into(),
            prefix: "fx".into(),
            properties: vec![XmpProperty {
                name: "DocumentType".into(),
                value: "INVOICE".into(),
                value_type: "Text".into(),
                category: "external".into(),
                description: "INVOICE".into(),
            }],
        };
        let out = extend(xmp, &ext).unwrap();
        assert!(out.contains("xmlns:fx=\"urn:factur-x:pdfa:CrossIndustryDocument:invoice:1p0#\""));
        assert!(out.contains("<fx:DocumentType>INVOICE</fx:DocumentType></rdf:Description>"));
        assert!(out.contains("<rdf:Bag><rdf:li rdf:parseType=\"Resource\"><pdfaSchema:schema>"));
        assert_eq!(out.matches("<pdfaExtension:schemas>").count(), 1);
    }

    #[test]
    fn a_pdf_keeps_its_cross_references_when_its_xmp_grows() {
        let ext = XmpExtension {
            schema: "Test schema".into(),
            namespace: "urn:test#".into(),
            prefix: "tt".into(),
            properties: vec![XmpProperty {
                name: "Thing".into(),
                value: "x".repeat(5000),
                value_type: "Text".into(),
                category: "external".into(),
                description: "a long value".into(),
            }],
        };
        let pdf = crate::pdf::render_with(
            "<html><body><p>hello</p></body></html>",
            "T",
            vec![],
            Some(&ext),
        )
        .unwrap();
        assert!(find(&pdf, b"<tt:Thing>", 0).is_some());
        // Every object the table names starts where it says.
        let at = rfind(&pdf, b"startxref", pdf.len()).unwrap();
        let tail = String::from_utf8_lossy(&pdf[at + 9..]).into_owned();
        let xref: usize = tail.split_whitespace().next().unwrap().parse().unwrap();
        assert_eq!(&pdf[xref..xref + 4], b"xref");
        let table = String::from_utf8_lossy(&pdf[xref..]).into_owned();
        let mut lines = table.lines().skip(1);
        let header: Vec<usize> = lines
            .next()
            .unwrap()
            .split_whitespace()
            .map(|t| t.parse().unwrap())
            .collect();
        for (n, line) in lines.take(header[1]).enumerate() {
            if !line.trim_end().ends_with('n') {
                continue;
            }
            let offset: usize = line[..10].parse().unwrap();
            let expected = format!("{} 0 obj", header[0] + n);
            assert!(
                pdf[offset..].starts_with(expected.as_bytes()),
                "object {} is at {offset}",
                header[0] + n
            );
        }
        // The stream's length is the packet's.
        let start = find(&pdf, b"<x:xmpmeta", 0).unwrap();
        let end = find(&pdf, b"</x:xmpmeta>", start).unwrap() + b"</x:xmpmeta>".len();
        let object = rfind(&pdf, b" obj", start).unwrap();
        let dict = String::from_utf8_lossy(&pdf[object..start]).into_owned();
        let length: usize = dict[dict.find("/Length ").unwrap() + 8..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap();
        // The stream holds exactly that many bytes, the packet among them.
        let data = find(&pdf, b"stream", object).unwrap() + b"stream".len();
        let data = data + if pdf[data] == b'\r' { 2 } else { 1 };
        assert!(data <= start && end <= data + length);
        assert!(
            pdf[data + length..].starts_with(b"\nendstream")
                || pdf[data + length..].starts_with(b"endstream")
                || pdf[data + length..].starts_with(b"\r\nendstream")
        );
    }
}
