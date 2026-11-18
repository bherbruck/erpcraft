//! A small, bounded XML tree for invoices, read with `quick-xml`, and a
//! writer that escapes everything it is given.
//!
//! What is refused when reading: a document type declaration (so no DTD,
//! no entity of its own and nothing fetched from outside), any entity but
//! the five XML defines and character references, more than
//! [`MAX_BYTES`], more than [`MAX_NODES`] elements, and nesting deeper than
//! [`MAX_DEPTH`]. Nothing is expanded, resolved or fetched.

use quick_xml::events::Event;

/// The largest XML accepted: an invoice of thousands of lines is far
/// smaller.
pub const MAX_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_NODES: usize = 200_000;
pub const MAX_DEPTH: usize = 64;

/// An element: its local name, the prefix it was written with, its
/// attributes, its text and its child elements.
#[derive(Debug, Clone, Default)]
pub struct Node {
    pub name: String,
    pub prefix: Option<String>,
    /// Attributes by their name as written (`xmlns:cbc`, `currencyID`).
    pub attrs: Vec<(String, String)>,
    /// The text directly inside, trimmed.
    pub text: String,
    pub children: Vec<Node>,
    /// The line the element starts on, for messages.
    pub line: usize,
}

impl Node {
    /// The first child of that local name.
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Every child of that local name.
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// The first element down this path of local names.
    pub fn at(&self, path: &[&str]) -> Option<&Node> {
        path.iter().try_fold(self, |node, name| node.child(name))
    }

    /// The text down this path, if there is any.
    pub fn text_at(&self, path: &[&str]) -> Option<String> {
        self.at(path)
            .map(|n| n.text.clone())
            .filter(|t| !t.is_empty())
    }

    /// An attribute by its local name (`currencyID`, `schemeID`).
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.rsplit(':').next() == Some(name) && !k.starts_with("xmlns"))
            .map(|(_, v)| v.as_str())
    }

    /// The namespace this element's prefix is bound to on itself.
    pub fn namespace(&self) -> Option<&str> {
        let key = match &self.prefix {
            Some(prefix) => format!("xmlns:{prefix}"),
            None => "xmlns".to_owned(),
        };
        self.attrs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Counts lines up to a position, going forward from where it was last
/// asked, so that a whole document is scanned once.
struct Lines<'a> {
    content: &'a [u8],
    at: usize,
    line: usize,
}

impl Lines<'_> {
    fn at(&mut self, position: u64) -> usize {
        let end = (position as usize).min(self.content.len());
        if end < self.at {
            self.at = 0;
            self.line = 1;
        }
        self.line += self.content[self.at..end]
            .iter()
            .filter(|b| **b == b'\n')
            .count();
        self.at = end;
        self.line
    }
}

/// Read a document into its root element.
pub fn parse(content: &str) -> Result<Node, String> {
    if content.len() > MAX_BYTES {
        return Err(format!(
            "the file is larger than {} MiB",
            MAX_BYTES / 1024 / 1024
        ));
    }
    let mut reader = quick_xml::Reader::from_str(content);
    let mut lines = Lines {
        content: content.as_bytes(),
        at: 0,
        line: 1,
    };
    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;
    let mut nodes = 0usize;
    let open = |start: &quick_xml::events::BytesStart, line: usize| -> Result<Node, String> {
        let qualified = start.name().as_ref().to_owned();
        let (prefix, name) = match qualified.split_once(':') {
            Some((p, n)) => (Some(p.to_owned()), n.to_owned()),
            None => (None, qualified),
        };
        let mut attrs = Vec::new();
        for attr in start.attributes() {
            let attr = attr.map_err(|e| format!("line {line}: a broken attribute: {e}"))?;
            let key = attr.key.as_ref().to_owned();
            let value = attr
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_err(|e| format!("line {line}: attribute {key}: {e}"))?
                .into_owned();
            attrs.push((key, value));
        }
        Ok(Node {
            name,
            prefix,
            attrs,
            line,
            ..Node::default()
        })
    };
    loop {
        let position = reader.buffer_position();
        let event = reader.read_event().map_err(|e| {
            format!(
                "line {}: the file is not well-formed XML: {e}",
                Lines {
                    content: content.as_bytes(),
                    at: 0,
                    line: 1
                }
                .at(reader.error_position())
            )
        })?;
        let line = lines.at(position);
        match event {
            Event::DocType(_) => {
                return Err(format!(
                    "line {line}: a document type declaration (DTD) is not accepted"
                ));
            }
            Event::Start(start) => {
                nodes += 1;
                if nodes > MAX_NODES {
                    return Err(format!("the file has more than {MAX_NODES} elements"));
                }
                if stack.len() >= MAX_DEPTH {
                    return Err(format!(
                        "line {line}: elements nest deeper than {MAX_DEPTH} levels"
                    ));
                }
                if root.is_some() {
                    return Err(format!("line {line}: there is more than one root element"));
                }
                stack.push(open(&start, line)?);
            }
            Event::Empty(start) => {
                nodes += 1;
                if nodes > MAX_NODES {
                    return Err(format!("the file has more than {MAX_NODES} elements"));
                }
                let node = open(&start, line)?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(node),
                    None if root.is_none() => root = Some(node),
                    None => {
                        return Err(format!("line {line}: there is more than one root element"));
                    }
                }
            }
            Event::Text(t) => {
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&t.xml10_content());
                } else if !t.xml10_content().trim().is_empty() {
                    return Err(format!("line {line}: text outside the root element"));
                }
            }
            Event::CData(c) => {
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&c.into_inner());
                }
            }
            Event::GeneralRef(r) => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => Some(c),
                    _ => match r.into_inner().as_ref() {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        _ => None,
                    },
                };
                match (resolved, stack.last_mut()) {
                    (Some(c), Some(node)) => node.text.push(c),
                    _ => return Err(format!("line {line}: an unknown entity is not accepted")),
                }
            }
            Event::End(_) => {
                let mut node = stack
                    .pop()
                    .ok_or_else(|| format!("line {line}: an end tag without its start"))?;
                node.text = node.text.trim().to_owned();
                match stack.last_mut() {
                    Some(parent) => parent.children.push(node),
                    None => root = Some(node),
                }
            }
            Event::Eof => break,
            // Comments, processing instructions, the declaration.
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err("the file ends inside an element".into());
    }
    root.ok_or_else(|| "the file holds no XML element".into())
}

/// Escape text for an element or an attribute value.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Characters XML 1.0 does not allow are left out.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            '\u{FFFE}' | '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

/// Writes indented XML. Elements without text are not written: an empty
/// element is an error in EN 16931 (PEPPOL-EN16931-R008).
#[derive(Default)]
pub struct Writer {
    out: String,
    open: Vec<String>,
}

impl Writer {
    pub fn new() -> Self {
        Writer {
            out: "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n".into(),
            open: Vec::new(),
        }
    }

    fn indent(&mut self) {
        for _ in 0..self.open.len() {
            self.out.push_str("  ");
        }
    }

    fn tag(name: &str, attrs: &[(&str, &str)]) -> String {
        let mut tag = name.to_owned();
        for (key, value) in attrs {
            tag.push_str(&format!(" {key}=\"{}\"", escape(value)));
        }
        tag
    }

    pub fn open(&mut self, name: &str, attrs: &[(&str, &str)]) {
        self.indent();
        self.out
            .push_str(&format!("<{}>\n", Self::tag(name, attrs)));
        self.open.push(name.to_owned());
    }

    pub fn close(&mut self) {
        if let Some(name) = self.open.pop() {
            self.indent();
            self.out.push_str(&format!("</{name}>\n"));
        }
    }

    /// An element with text; nothing when the text is empty.
    pub fn leaf(&mut self, name: &str, attrs: &[(&str, &str)], text: &str) {
        // Empty is judged on what is written, after what XML does not
        // allow is left out.
        let escaped = escape(text);
        if escaped.trim().is_empty() {
            return;
        }
        self.indent();
        self.out.push_str(&format!(
            "<{}>{}</{name}>\n",
            Self::tag(name, attrs),
            escaped
        ));
    }

    /// An element with text, if there is one.
    pub fn opt(&mut self, name: &str, text: Option<&str>) {
        if let Some(text) = text {
            self.leaf(name, &[], text);
        }
    }

    pub fn finish(mut self) -> String {
        while !self.open.is_empty() {
            self.close();
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_tree_and_refuses_what_could_expand() {
        let root = parse(
            "<?xml version=\"1.0\"?><a:R xmlns:a=\"urn:x\"><a:B c=\"1\">x &amp; y</a:B><C/></a:R>",
        )
        .unwrap();
        assert_eq!(root.name, "R");
        assert_eq!(root.namespace(), Some("urn:x"));
        assert_eq!(root.text_at(&["B"]).as_deref(), Some("x & y"));
        assert_eq!(root.at(&["B"]).unwrap().attr("c"), Some("1"));
        assert!(root.child("C").is_some());

        let bomb = "<!DOCTYPE r [<!ENTITY a \"aaaa\"><!ENTITY b \"&a;&a;\">]><r>&b;</r>";
        assert!(parse(bomb).unwrap_err().contains("DTD"));
        let external = "<!DOCTYPE r SYSTEM \"file:///etc/passwd\"><r/>";
        assert!(parse(external).unwrap_err().contains("DTD"));
        assert!(
            parse("<r>&unknown;</r>")
                .unwrap_err()
                .contains("unknown entity")
        );
        assert!(parse("<r><a></r>").is_err());
        let deep = "<a>".repeat(MAX_DEPTH + 1) + &"</a>".repeat(MAX_DEPTH + 1);
        assert!(parse(&deep).unwrap_err().contains("deeper"));
    }

    #[test]
    fn writes_escaped_and_leaves_out_empty_elements() {
        let mut w = Writer::new();
        w.open("r", &[("x", "a\"b")]);
        w.leaf("t", &[], "1 < 2 & 3");
        w.leaf("empty", &[], " ");
        w.leaf("controls", &[], "\u{1}\u{2} \u{FFFE}");
        w.leaf("kept", &[], "a\u{FFFF}b");
        let out = w.finish();
        assert!(out.contains("<r x=\"a&quot;b\">"));
        assert!(out.contains("<t>1 &lt; 2 &amp; 3</t>"));
        assert!(!out.contains("empty"));
        assert!(!out.contains("controls"), "{out}");
        assert!(out.contains("<kept>ab</kept>"));
        assert!(parse(&out).is_ok());
    }
}
