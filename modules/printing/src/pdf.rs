//! A printed page as a PDF, made on the server from the same HTML a
//! browser prints: no browser, no system program, nothing fetched.
//!
//! The page is the sanitised output of a print template, so it holds only
//! the elements and classes printing allows. This lays it out the way the
//! built-in stylesheet does, in the parts that matter on paper: blocks one
//! under the other; the letterhead, the parties and the footer as columns
//! side by side; tables with their columns sized to what they hold,
//! numbers to the right, rows that do not fit carried to the next page
//! with the table's head again; headings, bold, grey, lines; the logo. It
//! is not a CSS engine: what a template styles with its own `style`
//! attributes shows in a browser, not here.
//!
//! The PDF is written with `krilla` (MIT or Apache-2.0) as PDF/A-3b, with
//! Liberation Sans embedded (SIL Open Font License, see `fonts/`), so a
//! file can be attached to it: an EN 16931 invoice's XML makes it a
//! ZUGFeRD / Factur-X invoice.

use std::cell::RefCell;
use std::sync::OnceLock;

use base64::Engine;
use html5ever::tendril::TendrilSink;
use krilla::Document;
use krilla::SerializeSettings;
use krilla::color::rgb;
use krilla::configure::{Archival, ConfigurationBuilder};
use krilla::embed::{AssociationKind, EmbeddedFile, MimeType};
use krilla::geom::{PathBuilder, Point, Size};
use krilla::image::Image;
use krilla::metadata::{DateTime, Metadata};
use krilla::num::NormalizedF32;
use krilla::page::PageSettings;
use krilla::paint::{Fill, Stroke};
use krilla::text::{Font, TextDirection};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

const REGULAR: &[u8] = include_bytes!("../fonts/LiberationSans-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../fonts/LiberationSans-Bold.ttf");

/// Points per millimetre.
const MM: f32 = 72.0 / 25.4;
const PAGE_W: f32 = 210.0 * MM;
const PAGE_H: f32 = 297.0 * MM;
const MARGIN_X: f32 = 16.0 * MM;
const MARGIN_Y: f32 = 18.0 * MM;
/// The most pages one document makes.
const MAX_PAGES: usize = 500;

/// A file to carry inside the PDF, such as an invoice's XML.
pub struct Attachment {
    pub name: String,
    pub mime_type: String,
    pub description: String,
    pub data: Vec<u8>,
}

/// The PDF of a printed page, as PDF/A-3b, with `attachments` in it.
pub fn render(html: &str, title: &str, attachments: Vec<Attachment>) -> Result<Vec<u8>, String> {
    render_with(html, title, attachments, None)
}

/// [`render`], with an XMP extension schema in the PDF's metadata, such as
/// Factur-X's.
pub fn render_with(
    html: &str,
    title: &str,
    attachments: Vec<Attachment>,
    extension: Option<&crate::xmp::XmpExtension>,
) -> Result<Vec<u8>, String> {
    let dom = html5ever::parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .map_err(|e| format!("the page cannot be read: {e}"))?;
    let body = find(&dom.document, "body").ok_or("the page has no body")?;
    let mut layout = Layout::new();
    layout.block_children(&body, MARGIN_X, PAGE_W - 2.0 * MARGIN_X, Style::default());
    if layout.pages.len() > MAX_PAGES {
        return Err(format!("the document is longer than {MAX_PAGES} pages"));
    }
    let pdf = paint(&layout.pages, title, attachments)?;
    match extension {
        Some(extension) => crate::xmp::add_extension(&pdf, extension),
        None => Ok(pdf),
    }
}

// ------------------------------------------------------------------ fonts

struct Fonts {
    regular: Font,
    bold: Font,
    regular_face: ttf_parser::Face<'static>,
    bold_face: ttf_parser::Face<'static>,
}

fn fonts() -> &'static Fonts {
    static FONTS: OnceLock<Fonts> = OnceLock::new();
    FONTS.get_or_init(|| Fonts {
        regular: Font::new(REGULAR.into(), 0).expect("the bundled font loads"),
        bold: Font::new(BOLD.into(), 0).expect("the bundled font loads"),
        regular_face: ttf_parser::Face::parse(REGULAR, 0).expect("the bundled font parses"),
        bold_face: ttf_parser::Face::parse(BOLD, 0).expect("the bundled font parses"),
    })
}

/// How wide `text` is at `size` points.
fn width(text: &str, size: f32, bold: bool) -> f32 {
    let fonts = fonts();
    let face = if bold {
        &fonts.bold_face
    } else {
        &fonts.regular_face
    };
    let units = face.units_per_em() as f32;
    text.chars()
        .map(|c| {
            face.glyph_index(c)
                .and_then(|g| face.glyph_hor_advance(g))
                .unwrap_or(units as u16 / 2) as f32
        })
        .sum::<f32>()
        * size
        / units
}

// ------------------------------------------------------------------ style

#[derive(Debug, Clone, Copy, PartialEq)]
enum Align {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy)]
struct Style {
    size: f32,
    bold: bool,
    grey: bool,
    upper: bool,
    align: Align,
    /// Keep the text on one line: numbers.
    nowrap: bool,
    /// Keep line breaks as written.
    pre: bool,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            size: 10.0,
            bold: false,
            grey: false,
            upper: false,
            align: Align::Left,
            nowrap: false,
            pre: false,
        }
    }
}

fn tag(node: &Handle) -> Option<String> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.to_string()),
        _ => None,
    }
}

fn attr(node: &Handle, wanted: &str) -> Option<String> {
    match &node.data {
        NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .find(|a| &*a.name.local == wanted)
            .map(|a| a.value.to_string()),
        _ => None,
    }
}

fn has_class(node: &Handle, class: &str) -> bool {
    attr(node, "class").is_some_and(|c| c.split_whitespace().any(|x| x == class))
}

fn find(node: &Handle, name: &str) -> Option<Handle> {
    if tag(node).as_deref() == Some(name) {
        return Some(node.clone());
    }
    node.children.borrow().iter().find_map(|c| find(c, name))
}

/// The style an element gives its content.
fn styled(node: &Handle, outer: Style) -> Style {
    let mut s = outer;
    match tag(node).as_deref() {
        Some("h1") => {
            s.size = 15.0;
            s.bold = true;
        }
        Some("h2") => {
            s.size = 8.5;
            s.bold = true;
            s.grey = true;
            s.upper = true;
        }
        Some("h3" | "h4" | "h5" | "h6") => s.bold = true,
        Some("b" | "strong" | "th") => s.bold = true,
        Some("small") => s.size = outer.size * 0.85,
        _ => {}
    }
    if has_class(node, "company-name") {
        s.size = 16.0;
        s.bold = true;
    }
    if has_class(node, "party-name") {
        s.size = 11.0;
        s.bold = true;
    }
    if has_class(node, "muted") {
        s.grey = true;
    }
    if has_class(node, "num") {
        s.align = Align::Right;
        s.nowrap = true;
    }
    if has_class(node, "short") {
        s.nowrap = true;
    }
    if has_class(node, "memo") {
        s.pre = true;
    }
    if has_class(node, "foot") {
        s.size = 8.0;
        s.grey = true;
    }
    s
}

const INLINE: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "cite", "code", "data", "dfn", "em", "i", "kbd", "mark", "q",
    "s", "samp", "small", "span", "strong", "sub", "sup", "time", "u", "var", "wbr",
];

fn is_inline(node: &Handle) -> bool {
    match &node.data {
        NodeData::Text { .. } => true,
        NodeData::Element { name, .. } => {
            let n = &*name.local;
            INLINE.contains(&n) || n == "br"
        }
        _ => false,
    }
}

// ---------------------------------------------------------------- content

/// A piece of text with its style, or a line break.
#[derive(Debug, Clone)]
enum Run {
    Text(String, Style),
    Break,
}

fn runs_of(node: &Handle, style: Style, out: &mut Vec<Run>) {
    match &node.data {
        NodeData::Text { contents } => {
            let text = contents.borrow().to_string();
            if style.pre {
                for (i, part) in text.split('\n').enumerate() {
                    if i > 0 {
                        out.push(Run::Break);
                    }
                    if !part.is_empty() {
                        out.push(Run::Text(part.to_owned(), style));
                    }
                }
            } else {
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                let lead = contents.borrow().starts_with(char::is_whitespace);
                let trail = contents.borrow().ends_with(char::is_whitespace);
                let mut t = String::new();
                if lead {
                    t.push(' ');
                }
                t.push_str(&text);
                if trail && !text.is_empty() {
                    t.push(' ');
                }
                if !t.is_empty() {
                    out.push(Run::Text(t, style));
                }
            }
        }
        NodeData::Element { name, .. } if &*name.local == "br" => out.push(Run::Break),
        NodeData::Element { .. } => {
            let inner = styled(node, style);
            for child in node.children.borrow().iter() {
                runs_of(child, inner, out);
            }
        }
        _ => {}
    }
}

/// A line of text, ready to draw.
#[derive(Debug, Clone)]
struct Line {
    pieces: Vec<(String, Style)>,
    width: f32,
    height: f32,
}

/// Lines of `runs` wrapped to `width` (or kept whole when `width` is
/// `None`).
fn wrap(runs: &[Run], width: Option<f32>) -> Vec<Line> {
    let mut lines = vec![];
    let mut line = Line {
        pieces: vec![],
        width: 0.0,
        height: 0.0,
    };
    let flush = |line: &mut Line, lines: &mut Vec<Line>| {
        // A line does not begin or end with a space.
        if let Some((text, _)) = line.pieces.first_mut() {
            *text = text.trim_start().to_owned();
        }
        if let Some((text, _)) = line.pieces.last_mut() {
            *text = text.trim_end().to_owned();
        }
        line.pieces.retain(|(t, _)| !t.is_empty());
        line.width = line.pieces.iter().map(|(t, s)| width_of(t, s)).sum();
        lines.push(std::mem::replace(
            line,
            Line {
                pieces: vec![],
                width: 0.0,
                height: 0.0,
            },
        ));
    };
    for run in runs {
        match run {
            Run::Break => {
                if line.height == 0.0 {
                    line.height = 10.0 * 1.35;
                }
                flush(&mut line, &mut lines);
            }
            Run::Text(text, style) => {
                line.height = line.height.max(style.size * 1.35);
                let limit = if style.nowrap { None } else { width };
                // Words, each with the space before it.
                let mut word = String::new();
                let mut words = vec![];
                for c in text.chars() {
                    if c == ' ' && !word.trim().is_empty() {
                        words.push(std::mem::take(&mut word));
                    }
                    word.push(c);
                }
                if !word.is_empty() {
                    words.push(word);
                }
                for w in words {
                    let ww = width_of(&w, style);
                    if let Some(limit) = limit
                        && line.width + ww > limit
                        && line.width > 0.0
                    {
                        flush(&mut line, &mut lines);
                        line.height = style.size * 1.35;
                    }
                    let start = line.width == 0.0;
                    let w = if start { w.trim_start().to_owned() } else { w };
                    line.width += width_of(&w, style);
                    match line.pieces.last_mut() {
                        Some((t, s)) if same(s, style) => t.push_str(&w),
                        _ => line.pieces.push((w, *style)),
                    }
                }
            }
        }
    }
    if !line.pieces.is_empty() {
        flush(&mut line, &mut lines);
    }
    lines
}

fn same(a: &Style, b: &Style) -> bool {
    a.size == b.size && a.bold == b.bold && a.grey == b.grey && a.upper == b.upper
}

fn width_of(text: &str, style: &Style) -> f32 {
    if style.upper {
        width(&text.to_uppercase(), style.size, style.bold)
    } else {
        width(text, style.size, style.bold)
    }
}

// ----------------------------------------------------------------- layout

#[derive(Debug, Clone)]
enum Op {
    Text {
        x: f32,
        y: f32,
        text: String,
        style: Style,
    },
    Rule {
        x1: f32,
        x2: f32,
        y: f32,
        thick: f32,
        grey: f32,
    },
    Image {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        data: std::rc::Rc<(String, Vec<u8>)>,
    },
}

struct Layout {
    pages: Vec<Vec<Op>>,
    y: f32,
    /// Columns are laid out without breaking the page.
    no_break: bool,
}

impl Layout {
    fn new() -> Self {
        Layout {
            pages: vec![vec![]],
            y: MARGIN_Y,
            no_break: false,
        }
    }

    fn ops(&mut self) -> &mut Vec<Op> {
        self.pages.last_mut().expect("a page")
    }

    /// Room for `height` more points, on a new page if need be. True when
    /// a page was begun.
    fn room(&mut self, height: f32) -> bool {
        if self.no_break || self.y + height <= PAGE_H - MARGIN_Y || self.y <= MARGIN_Y + 1.0 {
            return false;
        }
        self.pages.push(vec![]);
        self.y = MARGIN_Y;
        true
    }

    fn lines(&mut self, lines: &[Line], x: f32, w: f32, align: Align) {
        for line in lines {
            self.room(line.height);
            let baseline = self.y + line.height * 0.78;
            let mut lx = match align {
                Align::Left => x,
                Align::Right => x + w - line.width,
            };
            for (text, style) in &line.pieces {
                let shown = if style.upper {
                    text.to_uppercase()
                } else {
                    text.clone()
                };
                let ww = width_of(text, style);
                self.ops().push(Op::Text {
                    x: lx,
                    y: baseline,
                    text: shown,
                    style: *style,
                });
                lx += ww;
            }
            self.y += line.height;
        }
    }

    fn paragraph(&mut self, runs: &[Run], x: f32, w: f32, style: Style) {
        if runs
            .iter()
            .all(|r| matches!(r, Run::Text(t, _) if t.trim().is_empty()))
        {
            return;
        }
        let lines = wrap(runs, Some(w));
        self.lines(&lines, x, w, style.align);
    }

    fn rule(&mut self, x1: f32, x2: f32, thick: f32, grey: f32) {
        let y = self.y;
        self.ops().push(Op::Rule {
            x1,
            x2,
            y,
            thick,
            grey,
        });
    }

    /// The children of a block: runs of inline content as paragraphs,
    /// block elements as blocks.
    fn block_children(&mut self, node: &Handle, x: f32, w: f32, style: Style) {
        let mut runs = vec![];
        for child in node.children.borrow().iter() {
            if is_inline(child) {
                runs_of(child, style, &mut runs);
            } else if tag(child).is_some() {
                self.paragraph(&std::mem::take(&mut runs), x, w, style);
                self.element(child, x, w, style);
            }
        }
        self.paragraph(&runs, x, w, style);
    }

    fn element(&mut self, node: &Handle, x: f32, w: f32, outer: Style) {
        let name = tag(node).unwrap_or_default();
        let style = styled(node, outer);
        match name.as_str() {
            "table" => return self.table(node, x, w, style),
            "img" => return self.image(node, x),
            "hr" => {
                self.y += 2.0 * MM;
                self.rule(x, x + w, 0.5, 0.6);
                self.y += 2.0 * MM;
                return;
            }
            "style" | "script" | "title" | "head" => return,
            _ => {}
        }
        let before = if has_class(node, "memo") {
            6.0 * MM
        } else if has_class(node, "foot") {
            10.0 * MM
        } else if has_class(node, "totals") {
            4.0 * MM
        } else if has_class(node, "signature") {
            18.0 * MM
        } else {
            0.0
        };
        self.y += before;
        if has_class(node, "foot") {
            self.rule(x, x + w, 0.5, 0.85);
            self.y += 2.0 * MM;
        }
        let columns = ["head", "parties", "foot", "signature"]
            .iter()
            .any(|c| has_class(node, c));
        if columns {
            self.columns(node, x, w, style);
        } else if name == "li" {
            let mut runs = vec![Run::Text("• ".into(), style)];
            for child in node.children.borrow().iter() {
                runs_of(child, style, &mut runs);
            }
            self.paragraph(&runs, x + 3.0 * MM, w - 3.0 * MM, style);
        } else {
            self.block_children(node, x, w, style);
        }
        let after = match name.as_str() {
            "h1" => 2.0 * MM,
            "h2" => 1.0 * MM,
            "p" => 2.0 * MM,
            _ if has_class(node, "head") || has_class(node, "parties") => 6.0 * MM,
            _ => 0.0,
        };
        if has_class(node, "head") {
            self.y += 2.0 * MM;
            self.rule(x, x + w, 1.5, 0.0);
        }
        self.y += after;
    }

    /// The element's child blocks side by side, in equal columns; the last
    /// of a page head to the right.
    fn columns(&mut self, node: &Handle, x: f32, w: f32, style: Style) {
        let children: Vec<Handle> = node
            .children
            .borrow()
            .iter()
            .filter(|c| tag(c).is_some())
            .cloned()
            .collect();
        if children.is_empty() {
            return;
        }
        let gap = 8.0 * MM;
        let n = children.len() as f32;
        let cw = (w - gap * (n - 1.0)) / n;
        self.room(20.0 * MM);
        let top = self.y;
        let mut bottom = top;
        let was = self.no_break;
        self.no_break = true;
        for (i, child) in children.iter().enumerate() {
            self.y = top;
            let mut s = style;
            if has_class(node, "head") && i + 1 == children.len() && i > 0 {
                s.align = Align::Right;
            }
            let cx = x + i as f32 * (cw + gap);
            self.element(child, cx, cw, s);
            bottom = bottom.max(self.y);
        }
        self.no_break = was;
        self.y = bottom;
    }

    fn image(&mut self, node: &Handle, x: f32) {
        let Some(src) = attr(node, "src") else {
            return;
        };
        let Some(rest) = src.strip_prefix("data:") else {
            return;
        };
        let Some((kind, data)) = rest.split_once(";base64,") else {
            return;
        };
        if kind != "image/png" && kind != "image/jpeg" {
            return;
        }
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
            return;
        };
        let Some((iw, ih)) = image_size(kind, &bytes) else {
            return;
        };
        let max_h = 18.0 * MM;
        let max_w = 60.0 * MM;
        let scale = (max_h / ih as f32).min(max_w / iw as f32);
        let (w, h) = (iw as f32 * scale, ih as f32 * scale);
        self.room(h);
        let y = self.y;
        self.ops().push(Op::Image {
            x,
            y,
            w,
            h,
            data: std::rc::Rc::new((kind.to_owned(), bytes)),
        });
        self.y += h + 2.0 * MM;
    }

    fn table(&mut self, node: &Handle, x: f32, w: f32, style: Style) {
        struct Row {
            cells: Vec<(Vec<Run>, Style)>,
            head: bool,
            grand: bool,
        }
        let lines_table = has_class(node, "lines");
        let mut rows: Vec<Row> = vec![];
        fn collect(node: &Handle, style: Style, head: bool, rows: &mut Vec<Row>) {
            for child in node.children.borrow().iter() {
                match tag(child).as_deref() {
                    Some("thead") => collect(child, style, true, rows),
                    Some("tbody" | "tfoot") => collect(child, style, false, rows),
                    Some("tr") => {
                        let grand = has_class(child, "grand");
                        let mut cells = vec![];
                        for cell in child.children.borrow().iter() {
                            if let Some(t) = tag(cell)
                                && (t == "td" || t == "th")
                            {
                                let mut s = styled(cell, style);
                                if grand {
                                    s.bold = true;
                                }
                                if t == "th" && !grand && head {
                                    s.size = 8.5;
                                    s.grey = true;
                                    s.nowrap = true;
                                }
                                if t == "th" && !head {
                                    s.bold = grand;
                                    s.grey = !grand && !has_class(cell, "num");
                                }
                                let mut runs = vec![];
                                for c in cell.children.borrow().iter() {
                                    runs_of(c, s, &mut runs);
                                }
                                cells.push((runs, s));
                            }
                        }
                        rows.push(Row { cells, head, grand });
                    }
                    _ => {}
                }
            }
        }
        collect(node, style, false, &mut rows);
        let ncols = rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
        if ncols == 0 {
            return;
        }
        let pad = if lines_table { 2.0 * MM } else { 1.5 * MM };
        // What each column holds on one line, and whether it may wrap.
        let mut natural = vec![0.0f32; ncols];
        let mut fixed = vec![false; ncols];
        for row in &rows {
            for (i, (runs, s)) in row.cells.iter().enumerate() {
                let one = wrap(runs, None).iter().map(|l| l.width).fold(0.0, f32::max);
                natural[i] = natural[i].max(one + 2.0 * pad);
                if s.nowrap && !row.head {
                    fixed[i] = true;
                }
            }
        }
        let total: f32 = natural.iter().sum();
        let mut widths = natural.clone();
        if total > w {
            let fixed_w: f32 = (0..ncols).filter(|i| fixed[*i]).map(|i| natural[i]).sum();
            let flexible: f32 = total - fixed_w;
            let room = (w - fixed_w).max(ncols as f32 * 10.0 * MM);
            for i in 0..ncols {
                if !fixed[i] && flexible > 0.0 {
                    widths[i] = (natural[i] / flexible * room).max(12.0 * MM);
                }
            }
        } else if lines_table {
            // A table of lines spans the page: the widest text column takes
            // what is left.
            let widest = (0..ncols)
                .filter(|i| !fixed[*i])
                .max_by(|a, b| natural[*a].total_cmp(&natural[*b]))
                .unwrap_or(0);
            widths[widest] += w - total;
        }
        let tw: f32 = widths.iter().sum();
        // Facts and totals sit at the right, as their margin puts them.
        let tx = if !lines_table
            && (has_class(node, "facts")
                || has_class(node, "totals")
                || style.align == Align::Right)
        {
            x + w - tw
        } else {
            x
        };
        let head_rows: Vec<usize> = (0..rows.len()).filter(|i| rows[*i].head).collect();
        let draw_row = |layout: &mut Layout, row: &Row| {
            let wrapped: Vec<Vec<Line>> = row
                .cells
                .iter()
                .enumerate()
                .map(|(i, (runs, _))| wrap(runs, Some(widths[i] - 2.0 * pad)))
                .collect();
            let content = wrapped
                .iter()
                .map(|l| l.iter().map(|l| l.height).sum::<f32>())
                .fold(0.0, f32::max);
            let vpad = if lines_table { 1.5 * MM } else { 0.7 * MM };
            let h = content + 2.0 * vpad;
            let new_page = layout.room(h);
            (h, wrapped, vpad, new_page)
        };
        let mut i = 0;
        while i < rows.len() {
            let row = &rows[i];
            let (h, wrapped, vpad, new_page) = draw_row(self, row);
            if new_page && lines_table && !row.head {
                // The head again on the new page.
                for &hr in &head_rows {
                    let (hh, hw, hv, _) = draw_row(self, &rows[hr]);
                    self.cells(&rows[hr].cells, &hw, &widths, tx, pad, hv);
                    self.y += hh;
                    self.rule(tx, tx + tw, 0.75, 0.0);
                }
            }
            if row.grand {
                self.rule(tx, tx + tw, 1.5, 0.0);
            }
            self.cells(&row.cells, &wrapped, &widths, tx, pad, vpad);
            self.y += h;
            if lines_table {
                if row.head {
                    self.rule(tx, tx + tw, 0.75, 0.0);
                } else {
                    self.rule(tx, tx + tw, 0.5, 0.87);
                }
            }
            i += 1;
        }
    }

    fn cells(
        &mut self,
        cells: &[(Vec<Run>, Style)],
        wrapped: &[Vec<Line>],
        widths: &[f32],
        x: f32,
        pad: f32,
        vpad: f32,
    ) {
        let top = self.y;
        let mut cx = x;
        for (i, ((_, style), lines)) in cells.iter().zip(wrapped).enumerate() {
            self.y = top + vpad;
            let was = self.no_break;
            self.no_break = true;
            self.lines(lines, cx + pad, widths[i] - 2.0 * pad, style.align);
            self.no_break = was;
            cx += widths[i];
        }
        self.y = top;
    }
}

fn image_size(kind: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    let image = match kind {
        "image/png" => Image::from_png(bytes.to_vec().into(), false).ok()?,
        _ => Image::from_jpeg(bytes.to_vec().into(), false).ok()?,
    };
    let (w, h) = image.size();
    (w > 0 && h > 0).then_some((w, h))
}

// ---------------------------------------------------------------- paint

fn paint(pages: &[Vec<Op>], title: &str, attachments: Vec<Attachment>) -> Result<Vec<u8>, String> {
    let fonts = fonts();
    let configuration = ConfigurationBuilder::new()
        .with_archival_validator(Archival::A3_B)
        .finish()
        .map_err(|e| format!("{e:?}"))?;
    let settings = SerializeSettings {
        configuration,
        ..SerializeSettings::default()
    };
    let mut document = Document::new_with(settings);
    let now = chrono_now();
    document.set_metadata(
        Metadata::new()
            .title(title.to_owned())
            .language("en".into())
            .creator("erpcraft".into())
            .producer("erpcraft".into())
            .creation_date(now),
    );
    for attachment in attachments {
        document
            .embed_file(EmbeddedFile {
                path: attachment.name,
                mime_type: MimeType::new(&attachment.mime_type),
                description: Some(attachment.description),
                association_kind: AssociationKind::Alternative,
                data: attachment.data.into(),
                modification_date: Some(now),
                compress: None,
                location: None,
            })
            .ok_or("a file with this name is attached already")?;
    }
    // Each image is written once, however often it is drawn.
    type Seen = Vec<(*const (String, Vec<u8>), Image)>;
    let images: RefCell<Seen> = RefCell::new(vec![]);
    for ops in pages {
        let mut page = document.start_page_with(
            PageSettings::from_wh(PAGE_W, PAGE_H).ok_or("the page size is invalid")?,
        );
        let mut surface = page.surface();
        for op in ops {
            match op {
                Op::Text { x, y, text, style } => {
                    let shade = if style.grey { 85 } else { 17 };
                    surface.set_fill(Some(Fill {
                        paint: rgb::Color::new(shade, shade, shade).into(),
                        opacity: NormalizedF32::ONE,
                        rule: Default::default(),
                    }));
                    let font = if style.bold {
                        fonts.bold.clone()
                    } else {
                        fonts.regular.clone()
                    };
                    surface.draw_text(
                        Point::from_xy(*x, *y),
                        font,
                        style.size,
                        text,
                        false,
                        TextDirection::Auto,
                    );
                }
                Op::Rule {
                    x1,
                    x2,
                    y,
                    thick,
                    grey,
                } => {
                    let mut pb = PathBuilder::new();
                    pb.move_to(*x1, *y);
                    pb.line_to(*x2, *y);
                    if let Some(path) = pb.finish() {
                        let shade = (grey * 255.0) as u8;
                        surface.set_fill(None);
                        surface.set_stroke(Some(Stroke {
                            paint: rgb::Color::new(shade, shade, shade).into(),
                            width: *thick,
                            ..Stroke::default()
                        }));
                        surface.draw_path(&path);
                        surface.set_stroke(None);
                    }
                }
                Op::Image { x, y, w, h, data } => {
                    let key = std::rc::Rc::as_ptr(data);
                    let found = images
                        .borrow()
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, i)| i.clone());
                    let image = match found {
                        Some(image) => image,
                        None => {
                            let (kind, bytes) = &**data;
                            let image = if kind == "image/png" {
                                Image::from_png(bytes.clone().into(), false)
                            } else {
                                Image::from_jpeg(bytes.clone().into(), false)
                            }?;
                            images.borrow_mut().push((key, image.clone()));
                            image
                        }
                    };
                    surface.push_transform(&krilla::geom::Transform::from_translate(*x, *y));
                    surface.draw_image(image, Size::from_wh(*w, *h).ok_or("an empty image")?);
                    surface.pop();
                }
            }
        }
        surface.finish();
        page.finish();
    }
    document.finish().map_err(|e| format!("{e:?}"))
}

fn chrono_now() -> DateTime {
    use chrono::{Datelike, Timelike};
    let now = chrono::Utc::now();
    DateTime::new(now.year() as u16)
        .month(now.month() as u8)
        .day(now.day() as u8)
        .hour(now.hour() as u8)
        .minute(now.minute() as u8)
        .second(now.second() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_becomes_a_pdf_with_its_text() {
        let html = "<!doctype html><html><head><title>T</title></head><body>\
            <div class=\"page\"><header class=\"head\"><div><div class=\"company-name\">ACME \
            GmbH</div><div class=\"address\">Hauptstraße 1<br>80331 München</div></div>\
            <div><h1>Invoice</h1></div></header>\
            <table class=\"lines\"><thead><tr><th>Item</th><th class=\"num\">Amount</th></tr>\
            </thead><tbody><tr><td>Widget</td><td class=\"num\">250.00</td></tr></tbody></table>\
            </div></body></html>";
        let pdf = render(html, "Invoice SI-1", vec![]).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(pdf.len() > 1000);
    }

    #[test]
    fn a_long_table_runs_onto_more_pages() {
        let rows: String = (0..200)
            .map(|i| format!("<tr><td>Line {i}</td><td class=\"num\">{i}.00</td></tr>"))
            .collect();
        let html = format!(
            "<html><body><table class=\"lines\"><thead><tr><th>Item</th><th>Amount</th></tr>\
             </thead><tbody>{rows}</tbody></table></body></html>"
        );
        let dom = html5ever::parse_document(RcDom::default(), Default::default())
            .from_utf8()
            .read_from(&mut html.as_bytes())
            .unwrap();
        let body = find(&dom.document, "body").unwrap();
        let mut layout = Layout::new();
        layout.block_children(&body, MARGIN_X, PAGE_W - 2.0 * MARGIN_X, Style::default());
        assert!(layout.pages.len() >= 3, "{} pages", layout.pages.len());
        // Every page after the first starts with the table's head again.
        for page in &layout.pages[1..] {
            assert!(
                matches!(&page[0], Op::Text { text, .. } if text == "Item"),
                "{:?}",
                page.first()
            );
        }
    }

    #[test]
    fn an_attachment_is_carried_inside() {
        let pdf = render(
            "<html><body><p>x</p></body></html>",
            "With XML",
            vec![Attachment {
                name: "factur-x.xml".into(),
                mime_type: "text/xml".into(),
                description: "Factur-X invoice".into(),
                data: b"<rsm:CrossIndustryInvoice/>".to_vec(),
            }],
        )
        .unwrap();
        let text = String::from_utf8_lossy(&pdf);
        assert!(
            text.contains("factur-x.xml"),
            "the file is named in the PDF"
        );
        assert!(text.contains("pdfaid"), "PDF/A metadata is written");
    }
}
