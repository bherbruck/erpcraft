//! The one tokenizer for ANSI X12 and UN/EDIFACT.
//!
//! Both standards are the same shape: segments ended by a terminator, a
//! tag, data elements, components inside an element, and (in newer
//! versions) repetitions of an element. They differ in which characters
//! separate what, where the file says so (the ISA of X12, the UNA of
//! EDIFACT) and in that EDIFACT has a release character and X12 has none.
//! So the separators are data ([`Delimiters`]) and the tokenizer is one.
//!
//! The input is hostile until read. The tokenizer works on bytes, in one
//! pass, without recursion, in time and memory linear in the input, and
//! everything it builds is counted against a [`Budget`]. It refuses:
//! a segment that is not terminated (a file cut off in transit), a tag
//! that is not two or three capital letters or digits, a control
//! character inside data, a release character with nothing after it, and
//! anything beyond the limits below.

use std::fmt;

/// The largest file read. An order of thousands of lines is far smaller;
/// partners that send more send several interchanges.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Segments in one file, over all its interchanges.
pub const MAX_SEGMENTS: usize = 200_000;
/// Components in one file, over all its segments. An element that is not
/// a composite counts as one.
pub const MAX_COMPONENTS: usize = 500_000;
/// Components in one segment. No segment of either standard comes near.
pub const MAX_COMPONENTS_PER_SEGMENT: usize = 512;
/// Bytes in one component. X12 allows 264 in its longest text element and
/// EDIFACT 512.
pub const MAX_COMPONENT_BYTES: usize = 4096;

/// Why a file could not be read at all, and where. A file that can be
/// read but breaks a rule of its standard is not this: that is reported
/// in the acknowledgement (see `x12::ack`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdiError {
    /// A complete sentence, without the data it is about.
    pub message: String,
    /// The byte the problem is at, counted from 0.
    pub offset: usize,
}

impl EdiError {
    pub fn at(offset: usize, message: impl Into<String>) -> Self {
        EdiError {
            message: message.into(),
            offset,
        }
    }
}

impl fmt::Display for EdiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.offset)
    }
}

impl std::error::Error for EdiError {}

/// The separators of one interchange. Every one is a single byte below
/// 0x80, so a file can be split on them whether its text is ASCII,
/// ISO 8859-1 or UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delimiters {
    /// Between data elements: `*` by custom in X12, `+` in EDIFACT.
    pub element: u8,
    /// Between the components of a composite element.
    pub component: u8,
    /// Between repetitions of an element: X12 from version 00402 (ISA11),
    /// EDIFACT from syntax version 4.
    pub repetition: Option<u8>,
    /// Ends a segment.
    pub terminator: u8,
    /// EDIFACT's release character: the byte after it is data. X12 has
    /// none, so its data can never contain a separator.
    pub release: Option<u8>,
    /// The decimal mark in numbers. Always `.` in X12; EDIFACT's UNA may
    /// say `,`.
    pub decimal: u8,
}

impl Delimiters {
    /// What most X12 files use. A reader never assumes these: it takes
    /// them from the ISA.
    pub const X12: Delimiters = Delimiters {
        element: b'*',
        component: b'>',
        repetition: None,
        terminator: b'~',
        release: None,
        decimal: b'.',
    };

    /// EDIFACT's defaults for character sets UNOA and UNOB, which apply
    /// when a file has no UNA.
    pub const EDIFACT: Delimiters = Delimiters {
        element: b'+',
        component: b':',
        repetition: None,
        terminator: b'\'',
        release: Some(b'?'),
        decimal: b'.',
    };

    /// Refuses separators that cannot work: one that is a letter, a digit
    /// or a space, one above 0x7F, or two that are the same.
    pub fn check(&self, offset: usize) -> Result<(), EdiError> {
        let mut all = vec![self.element, self.component, self.terminator];
        all.extend(self.repetition);
        all.extend(self.release);
        for (i, b) in all.iter().enumerate() {
            if b.is_ascii_alphanumeric() || *b == b' ' || *b >= 0x80 {
                return Err(EdiError::at(
                    offset,
                    "a separator is a letter, a digit, a space or not ASCII",
                ));
            }
            if all[..i].contains(b) {
                return Err(EdiError::at(
                    offset,
                    "two separators are the same character",
                ));
            }
        }
        Ok(())
    }

    /// Whether a byte is one of these separators.
    pub fn is_separator(&self, b: u8) -> bool {
        b == self.element
            || b == self.component
            || b == self.terminator
            || self.repetition == Some(b)
            || self.release == Some(b)
    }
}

/// What a file may still use of the limits. One budget covers a whole
/// file, so many small interchanges cannot add up past them.
#[derive(Debug, Clone)]
pub struct Budget {
    segments: usize,
    components: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            segments: MAX_SEGMENTS,
            components: MAX_COMPONENTS,
        }
    }
}

/// One data element: its repetitions, each a list of components. An
/// element that is neither repeated nor a composite is the common case
/// and costs one string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Element {
    value: String,
    more: Option<Box<More>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct More {
    /// The further components of the first repetition.
    components: Vec<String>,
    /// The further repetitions.
    repetitions: Vec<Vec<String>>,
}

impl Element {
    pub fn simple(value: impl Into<String>) -> Self {
        Element {
            value: value.into(),
            more: None,
        }
    }

    pub fn composite<S: AsRef<str>>(components: &[S]) -> Self {
        Element::from_repetitions(vec![
            components.iter().map(|c| c.as_ref().to_owned()).collect(),
        ])
    }

    pub fn from_repetitions(mut repetitions: Vec<Vec<String>>) -> Self {
        if repetitions.is_empty() {
            return Element::default();
        }
        let mut first = repetitions.remove(0);
        let value = if first.is_empty() {
            String::new()
        } else {
            first.remove(0)
        };
        let more = (!first.is_empty() || !repetitions.is_empty()).then(|| {
            Box::new(More {
                components: first,
                repetitions,
            })
        });
        Element { value, more }
    }

    /// The first component of the first repetition: the whole value of a
    /// simple element.
    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn repetitions(&self) -> usize {
        1 + self.more.as_ref().map_or(0, |m| m.repetitions.len())
    }

    /// The components of one repetition, counted from 0.
    pub fn components(&self, repetition: usize) -> Vec<&str> {
        match (repetition, &self.more) {
            (0, None) => vec![self.value.as_str()],
            (0, Some(more)) => std::iter::once(self.value.as_str())
                .chain(more.components.iter().map(String::as_str))
                .collect(),
            (n, Some(more)) => more
                .repetitions
                .get(n - 1)
                .map(|r| r.iter().map(String::as_str).collect())
                .unwrap_or_default(),
            (_, None) => Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty() && self.more.is_none()
    }
}

/// One segment: its tag and its data elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub tag: String,
    pub elements: Vec<Element>,
    /// The byte the segment starts at, for messages. Zero in a segment
    /// made to be written.
    pub offset: usize,
}

impl Segment {
    /// A segment of simple elements, to write.
    pub fn new<S: AsRef<str>>(tag: &str, elements: &[S]) -> Self {
        Segment {
            tag: tag.to_owned(),
            elements: elements
                .iter()
                .map(|e| Element::simple(e.as_ref()))
                .collect(),
            offset: 0,
        }
    }

    /// Element `n`, counted from 1 as the standards count (`BEG03` is
    /// `get(3)`): its first component, or nothing when it is not there.
    pub fn get(&self, n: usize) -> &str {
        n.checked_sub(1)
            .and_then(|i| self.elements.get(i))
            .map_or("", Element::value)
    }

    /// Component `c` of element `n`, both counted from 1.
    pub fn component(&self, n: usize, c: usize) -> &str {
        n.checked_sub(1)
            .and_then(|i| self.elements.get(i))
            .zip(c.checked_sub(1))
            .and_then(|(e, c)| e.components(0).get(c).copied())
            .unwrap_or("")
    }

    pub fn element(&self, n: usize) -> Option<&Element> {
        n.checked_sub(1).and_then(|i| self.elements.get(i))
    }
}

/// How the bytes of a file are read as text. Decided once per file: UTF-8
/// when the whole file is valid UTF-8 (ASCII is), otherwise ISO 8859-1,
/// in which every byte is a character. Partners in North America send
/// ASCII; EDIFACT's UNOC is ISO 8859-1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    Utf8,
    Latin1,
}

impl Charset {
    pub fn of(bytes: &[u8]) -> Self {
        if std::str::from_utf8(bytes).is_ok() {
            Charset::Utf8
        } else {
            Charset::Latin1
        }
    }

    fn decode(self, bytes: &[u8]) -> String {
        match self {
            // The whole file was valid and separators are ASCII, so a
            // piece between two of them is valid too.
            Charset::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
            Charset::Latin1 => bytes.iter().map(|b| *b as char).collect(),
        }
    }
}

/// Reads segments one at a time from a position in a file.
pub struct Tokenizer<'a> {
    bytes: &'a [u8],
    pos: usize,
    delimiters: Delimiters,
    charset: Charset,
}

impl<'a> Tokenizer<'a> {
    pub fn new(bytes: &'a [u8], pos: usize, delimiters: Delimiters, charset: Charset) -> Self {
        Tokenizer {
            bytes,
            pos,
            delimiters,
            charset,
        }
    }

    /// Where the next segment would start.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// The next segment, or nothing at the end of the file. Line ends
    /// after a terminator are skipped, as is padding after the last
    /// segment.
    pub fn next(&mut self, budget: &mut Budget) -> Result<Option<Segment>, EdiError> {
        let d = self.delimiters;
        self.pos = skip_filler(self.bytes, self.pos, &d);
        if self.pos >= self.bytes.len() {
            return Ok(None);
        }
        let start = self.pos;
        if budget.segments == 0 {
            return Err(EdiError::at(
                start,
                format!("the file has more than {MAX_SEGMENTS} segments"),
            ));
        }
        budget.segments -= 1;

        let mut elements: Vec<Element> = Vec::new();
        let mut repetitions: Vec<Vec<String>> = Vec::new();
        let mut components: Vec<String> = Vec::new();
        let mut current: Vec<u8> = Vec::new();
        let mut count = 0usize;
        let mut i = start;
        loop {
            let Some(&b) = self.bytes.get(i) else {
                return Err(EdiError::at(
                    start,
                    "the file ends inside a segment: it was cut off",
                ));
            };
            if d.release == Some(b) {
                let Some(&next) = self.bytes.get(i + 1) else {
                    return Err(EdiError::at(i, "a release character ends the file"));
                };
                current.push(next);
                i += 2;
            } else if d.is_separator(b) {
                count += 1;
                if count > MAX_COMPONENTS_PER_SEGMENT {
                    return Err(EdiError::at(
                        start,
                        format!("a segment has more than {MAX_COMPONENTS_PER_SEGMENT} components"),
                    ));
                }
                if budget.components == 0 {
                    return Err(EdiError::at(
                        start,
                        format!("the file has more than {MAX_COMPONENTS} components"),
                    ));
                }
                budget.components -= 1;
                components.push(self.charset.decode(&current));
                current.clear();
                if b != d.component {
                    repetitions.push(std::mem::take(&mut components));
                    if d.repetition != Some(b) {
                        elements.push(Element::from_repetitions(std::mem::take(&mut repetitions)));
                    }
                }
                i += 1;
                if b == d.terminator {
                    break;
                }
            } else if b < 0x20 || b == 0x7F {
                return Err(EdiError::at(i, "a control character is inside the data"));
            } else {
                if current.len() >= MAX_COMPONENT_BYTES {
                    return Err(EdiError::at(
                        i,
                        format!("a value is longer than {MAX_COMPONENT_BYTES} bytes"),
                    ));
                }
                current.push(b);
                i += 1;
            }
        }
        self.pos = i;

        let tag = elements.remove(0);
        let name = tag.value();
        let plain = tag.more.is_none()
            && (2..=3).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        if !plain {
            return Err(EdiError::at(
                start,
                "a segment does not begin with a tag of two or three capital letters or digits",
            ));
        }
        Ok(Some(Segment {
            tag: tag.value,
            elements,
            offset: start,
        }))
    }
}

/// Skips line ends between segments, and padding (spaces, tabs, the DOS
/// end-of-file byte) when nothing but padding is left.
pub fn skip_filler(bytes: &[u8], mut pos: usize, d: &Delimiters) -> usize {
    while let Some(&b) = bytes.get(pos) {
        if (b == b'\r' || b == b'\n') && !d.is_separator(b) {
            pos += 1;
        } else {
            break;
        }
    }
    let padding = |b: &u8| matches!(*b, b' ' | b'\t' | b'\r' | b'\n' | 0x1A | 0x00);
    if bytes
        .get(pos..)
        .is_some_and(|rest| rest.iter().all(padding))
    {
        return bytes.len();
    }
    pos
}

/// Writes one segment. Empty elements at the end are left out, as both
/// standards ask. Where the syntax has a release character, a separator
/// in the data is released; where it has none (X12), data that contains a
/// separator is refused, never written: it would change what the partner
/// reads.
pub fn write_segment(out: &mut String, segment: &Segment, d: &Delimiters) -> Result<(), EdiError> {
    let escape = |out: &mut String, text: &str| -> Result<(), EdiError> {
        for ch in text.chars() {
            let sep = u8::try_from(ch).is_ok_and(|b| d.is_separator(b));
            if ch.is_control() {
                return Err(EdiError::at(
                    0,
                    format!("a value of segment {} has a control character", segment.tag),
                ));
            }
            match (sep, d.release) {
                (true, Some(release)) => {
                    out.push(release as char);
                    out.push(ch);
                }
                (true, None) => {
                    return Err(EdiError::at(
                        0,
                        format!(
                            "a value of segment {} contains the separator '{ch}', which this \
                             syntax cannot write inside data",
                            segment.tag
                        ),
                    ));
                }
                (false, _) => out.push(ch),
            }
        }
        Ok(())
    };
    out.push_str(&segment.tag);
    let last = segment
        .elements
        .iter()
        .rposition(|e| !e.is_empty())
        .map_or(0, |i| i + 1);
    for element in &segment.elements[..last] {
        out.push(d.element as char);
        for r in 0..element.repetitions() {
            if r > 0 {
                let Some(rep) = d.repetition else {
                    return Err(EdiError::at(
                        0,
                        format!(
                            "segment {} repeats an element, which this version cannot write",
                            segment.tag
                        ),
                    ));
                };
                out.push(rep as char);
            }
            let components = element.components(r);
            let last = components
                .iter()
                .rposition(|c| !c.is_empty())
                .map_or(0, |i| i + 1);
            for (c, text) in components[..last].iter().enumerate() {
                if c > 0 {
                    out.push(d.component as char);
                }
                escape(out, text)?;
            }
        }
    }
    out.push(d.terminator as char);
    Ok(())
}
