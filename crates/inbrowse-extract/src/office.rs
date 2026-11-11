//! DOCX, PPTX, ODT and ODP text, read from the zipped XML with quick-xml.
//! Called by `extract_with_limits` (lib.rs); `open_archive` is also used by
//! detect.rs and sheet.rs so every archive is held to the same [`Limits`].
//!
//! Besides the main text this reads what hangs off it through the package's
//! relationships: Word headers, footers, footnotes, endnotes and comments, and
//! PowerPoint speaker notes (ODP keeps notes inside the slide, which is split
//! apart here). Tables come out as tab-separated rows (`xml_text`). A part
//! that cannot be read is skipped with a warning; main text that cannot be
//! read is an error.

use std::borrow::Cow;
use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::ops::Range;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::{ExtractError, Format, Limits, Section, SectionKind};

pub(crate) type Archive<'a> = zip::ZipArchive<Cursor<&'a [u8]>>;

/// Most columns one repeated or merged table cell may stand for. The counts
/// come from the file, and a table is rendered cell by cell.
const MAX_CELL_REPEAT: usize = 1024;

/// Deepest table inside a table cell that is laid out as a table. A cell's text
/// is folded again as each table around it closes, so nesting a hostile file
/// controls would cost time quadratic in its depth; deeper tables are read as
/// plain text of the cell they are in.
const MAX_TABLE_DEPTH: usize = 32;

pub(crate) fn open_archive<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Archive<'a>, ExtractError> {
    let archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| ExtractError::Malformed(format!("zip archive: {e}")))?;
    if archive.len() > limits.max_archive_entries {
        return Err(ExtractError::TooLarge(format!(
            "archive lists {} entries, limit {}",
            archive.len(),
            limits.max_archive_entries
        )));
    }
    Ok(archive)
}

/// Bytes still allowed, shared by every use of one budget.
struct Budget(u64);

/// An opened archive and the two budgets everything read from it draws on, so
/// that no number of parts can add up past [`Limits::max_unpacked_bytes`].
struct Package<'a> {
    archive: Archive<'a>,
    /// Bytes still allowed out of the archive, shared by every entry read.
    unpacked: Budget,
    /// Bytes of text still allowed out of the XML. A table cell repeated a
    /// thousand times costs a thousand cells of output for a few bytes of input.
    text: Budget,
}

/// One entry of a part's `.rels` file.
struct Relationship {
    id: String,
    /// The last segment of the relationship type, such as `header`.
    kind: String,
    /// Archive path of the target, resolved against the part that lists it.
    target: String,
}

impl Package<'_> {
    /// The entry `name` as UTF-8, counted against the unpacked budget.
    fn read(&mut self, name: &str) -> Result<String, ExtractError> {
        let entry = self
            .archive
            .by_name(name)
            .map_err(|e| ExtractError::Malformed(format!("{name}: {e}")))?;
        // The declared size can lie, so the read itself is capped as well.
        let mut raw = Vec::new();
        entry
            .take(self.unpacked.0.saturating_add(1))
            .read_to_end(&mut raw)
            .map_err(|e| ExtractError::Malformed(format!("{name}: {e}")))?;
        let read = raw.len() as u64;
        if read > self.unpacked.0 {
            return Err(ExtractError::TooLarge("archive unpacks past the size limit".into()));
        }
        self.unpacked.0 -= read;
        String::from_utf8(raw).map_err(|_| ExtractError::Malformed(format!("{name} is not UTF-8")))
    }

    /// The text of `xml`, which was read from `name`, counted against the text
    /// budget. Errors name the part.
    fn text(&mut self, name: &str, xml: &str, rules: &Rules) -> Result<String, ExtractError> {
        let text = xml_text(xml, rules, self.text.0).map_err(|e| match e {
            ExtractError::Malformed(why) => ExtractError::Malformed(format!("{name}: {why}")),
            other => other,
        })?;
        self.text.0 = self
            .text
            .0
            .checked_sub(text.len() as u64)
            .ok_or_else(|| ExtractError::TooLarge("extracted text passes the size limit".into()))?;
        Ok(text)
    }

    /// The text of a part that is not the main text: absent from the archive or
    /// unreadable means a warning and `None`, not a failed extraction. Limit
    /// errors still fail it. Sibling of `read_optional` and `relationships`.
    fn extra_text(
        &mut self,
        name: &str,
        rules: &Rules,
        warnings: &mut Vec<String>,
    ) -> Result<Option<String>, ExtractError> {
        if self.archive.index_for_name(name).is_none() {
            warnings.push(format!("part_missing: {name} is listed by the package but is not in the file"));
            return Ok(None);
        }
        let result = self.read(name).and_then(|xml| self.text(name, &xml, rules));
        tolerate(result, warnings)
    }

    /// A raw part that may legitimately be absent (`None`, silently); one that
    /// is present but unreadable is a warning and `None`.
    fn read_optional(&mut self, name: &str, warnings: &mut Vec<String>) -> Result<Option<String>, ExtractError> {
        if self.archive.index_for_name(name).is_none() {
            return Ok(None);
        }
        tolerate(self.read(name), warnings)
    }

    /// The relationships `part` lists in its `.rels` file. No such file means no
    /// relationships; an unreadable one is a warning and none.
    fn relationships(&mut self, part: &str, warnings: &mut Vec<String>) -> Result<Vec<Relationship>, ExtractError> {
        let rels = rels_path(part);
        let Some(xml) = self.read_optional(&rels, warnings)? else {
            return Ok(Vec::new());
        };
        let mut found = Vec::new();
        let parsed = for_each_element(&xml, "Relationship", |e| {
            let external = attribute(e, "TargetMode").is_some_and(|mode| mode == "External");
            if let (Some(id), Some(kind), Some(target), false) =
                (attribute(e, "Id"), attribute(e, "Type"), attribute(e, "Target"), external)
            {
                found.push(Relationship {
                    id,
                    kind: kind.rsplit('/').next().unwrap_or_default().to_string(),
                    target: resolve_target(part, &target),
                });
            }
        })
        .map_err(|e| match e {
            ExtractError::Malformed(why) => ExtractError::Malformed(format!("{rels}: {why}")),
            other => other,
        });
        Ok(tolerate(parsed, warnings)?.map(|()| found).unwrap_or_default())
    }
}

/// Turn a malformed part into a warning, so one bad header or notes page does
/// not cost the whole document. Limit and other errors still propagate.
fn tolerate<T>(result: Result<T, ExtractError>, warnings: &mut Vec<String>) -> Result<Option<T>, ExtractError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(ExtractError::Malformed(why)) => {
            warnings.push(format!("part_skipped: {why}"));
            Ok(None)
        }
        Err(other) => Err(other),
    }
}

/// `dir/file.xml` -> `dir/_rels/file.xml.rels`.
fn rels_path(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

/// The archive path a relationship `target` names when listed by `part`:
/// relative to the part's folder, or from the archive root when it starts with
/// `/`, with `.` and `..` segments resolved.
fn resolve_target(part: &str, target: &str) -> String {
    let joined = match target.strip_prefix('/') {
        Some(absolute) => absolute.to_string(),
        None => match part.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/{target}"),
            None => target.to_string(),
        },
    };
    let mut path = Vec::new();
    for segment in joined.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                path.pop();
            }
            other => path.push(other),
        }
    }
    path.join("/")
}

pub(crate) fn extract(
    bytes: &[u8],
    format: Format,
    limits: &Limits,
    warnings: &mut Vec<String>,
) -> Result<Vec<Section>, ExtractError> {
    let mut package = Package {
        archive: open_archive(bytes, limits)?,
        unpacked: Budget(limits.max_unpacked_bytes),
        text: Budget(limits.max_unpacked_bytes),
    };
    match format {
        Format::Docx => docx(&mut package, warnings),
        Format::Pptx => pptx(&mut package, warnings),
        Format::Odt => {
            let xml = package.read("content.xml")?;
            Ok(vec![body(package.text("content.xml", &xml, &ODF)?)])
        }
        Format::Odp => {
            let xml = package.read("content.xml")?;
            odp_slides(&mut package, &xml)
        }
        other => unreachable!("office::extract called for {other:?}"),
    }
}

fn body(text: String) -> Section {
    Section { kind: SectionKind::Body, name: None, text }
}

/// The parts a Word document points at, as (relationship type, section kind,
/// section name), in the order they are listed after the body.
const DOCX_EXTRA_PARTS: [(&str, SectionKind, &str); 5] = [
    ("header", SectionKind::Header, "Header"),
    ("footer", SectionKind::Footer, "Footer"),
    ("footnotes", SectionKind::Footnotes, "Footnotes"),
    ("endnotes", SectionKind::Endnotes, "Endnotes"),
    ("comments", SectionKind::Comments, "Comments"),
];

/// The body first, then one section per kind of `DOCX_EXTRA_PARTS`. Several
/// headers or footers with different text are numbered; identical ones (the
/// same header repeated for each section of the document) and empty ones are
/// dropped. Sibling of `pptx` and the ODF branches in `extract`.
fn docx(package: &mut Package<'_>, warnings: &mut Vec<String>) -> Result<Vec<Section>, ExtractError> {
    const DOCUMENT: &str = "word/document.xml";
    let xml = package.read(DOCUMENT)?;
    let mut sections = vec![body(package.text(DOCUMENT, &xml, &OOXML_WORD)?)];
    let relationships = package.relationships(DOCUMENT, warnings)?;
    for (relationship, kind, label) in DOCX_EXTRA_PARTS {
        let mut texts: Vec<String> = Vec::new();
        let mut read = HashSet::new();
        for part in relationships.iter().filter(|r| r.kind == relationship) {
            if !read.insert(part.target.as_str()) {
                continue;
            }
            if let Some(text) = package.extra_text(&part.target, &OOXML_WORD, warnings)? {
                if !text.is_empty() && !texts.contains(&text) {
                    texts.push(text);
                }
            }
        }
        let numbered = texts.len() > 1;
        sections.extend(texts.into_iter().enumerate().map(|(i, text)| Section {
            kind,
            name: Some(if numbered { format!("{label} {}", i + 1) } else { label.to_string() }),
            text,
        }));
    }
    Ok(sections)
}

/// Each slide in presentation order, followed by its notes when it has any.
/// Sibling of `docx`.
fn pptx(package: &mut Package<'_>, warnings: &mut Vec<String>) -> Result<Vec<Section>, ExtractError> {
    let mut sections = Vec::new();
    for (name, part) in slide_parts(package, warnings)? {
        let xml = package.read(&part)?;
        let text = package.text(&part, &xml, &OOXML_DRAWING)?;
        sections.push(Section { kind: SectionKind::Slide, name: Some(name.clone()), text });
        let notes = package
            .relationships(&part, warnings)?
            .into_iter()
            .find(|r| r.kind == "notesSlide");
        if let Some(notes) = notes {
            let text = package.extra_text(&notes.target, &OOXML_DRAWING, warnings)?;
            if let Some(text) = text.filter(|text| !text.is_empty()) {
                sections.push(Section { kind: SectionKind::Notes, name: Some(format!("{name} notes")), text });
            }
        }
    }
    Ok(sections)
}

/// (section name, archive path) of every slide. The order is the one in
/// `ppt/presentation.xml`, which is what PowerPoint shows and need not follow
/// the file numbers; slides are then named by their position in it. A package
/// with no usable slide list falls back to the file numbers, and names by them.
fn slide_parts(package: &mut Package<'_>, warnings: &mut Vec<String>) -> Result<Vec<(String, String)>, ExtractError> {
    if let Some(listed) = listed_slides(package, warnings)? {
        return Ok(listed);
    }
    let mut numbered: Vec<(u32, String)> = package
        .archive
        .file_names()
        .filter_map(|name| {
            let name = name.ok()?;
            let number = name.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse().ok()?;
            Some((number, name.into_owned()))
        })
        .collect();
    numbered.sort_unstable();
    Ok(numbered
        .into_iter()
        .map(|(number, part)| (format!("Slide {number}"), part))
        .collect())
}

/// The slides `ppt/presentation.xml` lists, resolved through its relationships;
/// `None` when it lists none that can be read. A listed slide that is not in
/// the file is a warning, and keeps the numbers of the slides after it.
fn listed_slides(
    package: &mut Package<'_>,
    warnings: &mut Vec<String>,
) -> Result<Option<Vec<(String, String)>>, ExtractError> {
    const PRESENTATION: &str = "ppt/presentation.xml";
    let Some(xml) = package.read_optional(PRESENTATION, warnings)? else {
        return Ok(None);
    };
    let mut ids = Vec::new();
    let parsed = for_each_element(&xml, "sldId", |e| ids.extend(relationship_id(e))).map_err(|e| match e {
        ExtractError::Malformed(why) => ExtractError::Malformed(format!("{PRESENTATION}: {why}")),
        other => other,
    });
    if tolerate(parsed, warnings)?.is_none() || ids.is_empty() {
        return Ok(None);
    }
    let relationships = package.relationships(PRESENTATION, warnings)?;
    let mut slides = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let number = index + 1;
        match relationships.iter().find(|r| r.kind == "slide" && &r.id == id) {
            Some(slide) if package.archive.index_for_name(&slide.target).is_some() => {
                slides.push((format!("Slide {number}"), slide.target.clone()));
            }
            Some(slide) => warnings.push(format!(
                "part_missing: {} is listed by the package but is not in the file",
                slide.target
            )),
            None => warnings.push(format!(
                "part_missing: slide {number} is listed by {PRESENTATION} but has no relationship"
            )),
        }
    }
    Ok((!slides.is_empty()).then_some(slides))
}

/// Call `visit` for every element whose local name is `local`, start or empty
/// tag alike. Used for relationship lists and the slide list.
fn for_each_element(xml: &str, local: &str, mut visit: impl FnMut(&BytesStart<'_>)) -> Result<(), ExtractError> {
    let mut reader = Reader::from_str(xml);
    loop {
        let event = reader
            .read_event()
            .map_err(|e| ExtractError::Malformed(format!("XML at byte {}: {e}", reader.error_position())))?;
        match event {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == local => visit(&e),
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// Which elements carry text and structure, by local name.
struct Rules {
    /// Text is kept only inside these elements.
    text: &'static [&'static str],
    /// Each of these ends a line.
    paragraph: &'static [&'static str],
    tab: &'static [&'static str],
    line_break: &'static [&'static str],
    /// ODF `text:s`: `text:c` spaces, default one.
    spaces: Option<&'static str>,
    /// How tables are laid out, for formats that have them.
    table: Option<TableRules>,
    /// Elements whose whole content is dropped (ODF page-number fields and
    /// header/footer placeholders, which hold literal text like `<number>`).
    skip_elements: &'static [&'static str],
    /// Elements dropped by an attribute they or a child carry.
    skip_by_attr: &'static [AttrSkip],
}

/// Element names of a format's tables, by local name.
#[derive(Clone, Copy)]
struct TableRules {
    table: &'static str,
    row: &'static str,
    /// Cell elements, including the placeholders of merged cells.
    cells: &'static [&'static str],
    /// Word's `w:gridSpan` child of a cell: the cell stands for that many
    /// columns and no placeholders follow it, so empty cells are added. PowerPoint
    /// and ODF list a placeholder cell for every merged column instead.
    grid_span: Option<&'static str>,
    /// ODF attribute that repeats a cell.
    repeat: Option<&'static str>,
}

/// Drop the content of the innermost `scope` element that is, or contains, a
/// `marker` element with `attr` set to one of `values`. PowerPoint marks a
/// shape's role on a child (`p:sp` > `p:nvPr` > `p:ph type`); ODF marks a frame
/// on the frame itself (`draw:frame presentation:class`).
struct AttrSkip {
    scope: &'static str,
    marker: &'static str,
    attr: &'static str,
    values: &'static [&'static str],
}

const WORD_TABLE: TableRules = TableRules {
    table: "tbl",
    row: "tr",
    cells: &["tc"],
    grid_span: Some("gridSpan"),
    repeat: None,
};

const DRAWING_TABLE: TableRules = TableRules {
    table: "tbl",
    row: "tr",
    cells: &["tc"],
    grid_span: None,
    repeat: None,
};

const ODF_TABLE: TableRules = TableRules {
    table: "table",
    row: "table-row",
    cells: &["table-cell", "covered-table-cell"],
    grid_span: None,
    repeat: Some("number-columns-repeated"),
};

const OOXML_WORD: Rules = Rules {
    text: &["t"],
    paragraph: &["p"],
    tab: &["tab"],
    line_break: &["br", "cr"],
    spaces: None,
    table: Some(WORD_TABLE),
    skip_elements: &[],
    // Word stores its footnote separator lines as footnotes of their own.
    skip_by_attr: &[
        AttrSkip {
            scope: "footnote",
            marker: "footnote",
            attr: "type",
            values: &["separator", "continuationSeparator", "continuationNotice"],
        },
        AttrSkip {
            scope: "endnote",
            marker: "endnote",
            attr: "type",
            values: &["separator", "continuationSeparator", "continuationNotice"],
        },
    ],
};

const OOXML_DRAWING: Rules = Rules {
    text: &["t"],
    paragraph: &["p"],
    tab: &["tab"],
    line_break: &["br"],
    spaces: None,
    table: Some(DRAWING_TABLE),
    skip_elements: &[],
    // Slide number, header, footer and date placeholders repeat on every slide.
    skip_by_attr: &[AttrSkip {
        scope: "sp",
        marker: "ph",
        attr: "type",
        values: &["sldNum", "hdr", "ftr", "dt"],
    }],
};

const ODF: Rules = Rules {
    text: &["p", "h"],
    paragraph: &["p", "h"],
    tab: &["tab"],
    line_break: &["line-break"],
    spaces: Some("s"),
    table: Some(ODF_TABLE),
    skip_elements: &[],
    skip_by_attr: &[],
};

/// ODF for presentations: the slide-number and header/footer fields and frames
/// that carry the placeholder text `<number>`, `<footer>` and so on.
const ODP: Rules = Rules {
    skip_elements: &["page-number", "placeholder"],
    skip_by_attr: &[AttrSkip {
        scope: "frame",
        marker: "frame",
        attr: "class",
        values: &["page-number", "header", "footer", "date-time"],
    }],
    ..ODF
};

/// A table cell being filled.
struct OpenCell {
    text: String,
    /// How many columns this cell's text is repeated across.
    repeat: usize,
    /// Empty columns that follow it (a Word cell merged across columns).
    padding: usize,
}

/// A finished cell, `repeat` columns wide.
struct Cell {
    text: String,
    repeat: usize,
}

#[derive(Default)]
struct OpenTable {
    rows: Vec<Vec<Cell>>,
    row: Option<Vec<Cell>>,
    cell: Option<OpenCell>,
}

/// Where extracted text goes: the document, or the open cell of the innermost
/// table. Paragraph and line breaks inside a cell become newlines here and are
/// folded into spaces when the cell closes (`flatten_cell`), so a row stays on
/// one line.
#[derive(Default)]
struct Writer {
    out: String,
    tables: Vec<OpenTable>,
    /// Open tables nested past `MAX_TABLE_DEPTH`, whose structure is ignored.
    too_deep: usize,
}

impl Writer {
    fn sink(&mut self) -> &mut String {
        match self.tables.last_mut().and_then(|table| table.cell.as_mut()) {
            Some(cell) => &mut cell.text,
            None => &mut self.out,
        }
    }

    /// A table, row or cell element opens.
    fn open(&mut self, rules: &TableRules, name: &str, repeat: usize) {
        if self.too_deep > 0 || (name == rules.table && self.tables.len() >= MAX_TABLE_DEPTH) {
            if name == rules.table {
                self.too_deep += 1;
            }
        } else if name == rules.table {
            self.tables.push(OpenTable::default());
        } else if name == rules.row {
            if let Some(table) = self.tables.last_mut() {
                table.row = Some(Vec::new());
            }
        } else if rules.cells.contains(&name) {
            if let Some(table) = self.tables.last_mut() {
                table.cell = Some(OpenCell { text: String::new(), repeat, padding: 0 });
            }
        }
    }

    /// A cell's `gridSpan` child: this cell covers `span` columns.
    fn span(&mut self, span: usize) {
        if self.too_deep > 0 {
            return;
        }
        if let Some(cell) = self.tables.last_mut().and_then(|table| table.cell.as_mut()) {
            cell.padding = span.clamp(1, MAX_CELL_REPEAT) - 1;
        }
    }

    /// A table, row or cell element closes. `limit` is the most text the whole
    /// document may still produce.
    fn close(&mut self, rules: &TableRules, name: &str, limit: u64) -> Result<(), ExtractError> {
        if self.too_deep > 0 {
            if name == rules.table {
                self.too_deep -= 1;
            }
        } else if rules.cells.contains(&name) {
            let Some(table) = self.tables.last_mut() else { return Ok(()) };
            let Some(cell) = table.cell.take() else { return Ok(()) };
            let row = table.row.get_or_insert_with(Vec::new);
            row.push(Cell { text: flatten_cell(&cell.text), repeat: cell.repeat });
            if cell.padding > 0 {
                row.push(Cell { text: String::new(), repeat: cell.padding });
            }
        } else if name == rules.row {
            if let Some(table) = self.tables.last_mut() {
                if let Some(row) = table.row.take() {
                    table.rows.push(row);
                }
            }
        } else if name == rules.table {
            let Some(mut table) = self.tables.pop() else { return Ok(()) };
            table.rows.extend(table.row.take());
            let rendered = render(&table.rows, limit.saturating_sub(self.out.len() as u64))?;
            if rendered.is_empty() {
                return Ok(());
            }
            match self.tables.last_mut().and_then(|outer| outer.cell.as_mut()) {
                // A table inside a cell is folded into that cell's one line.
                Some(cell) => {
                    cell.text.push('\n');
                    cell.text.push_str(&rendered);
                }
                None => push_block(&mut self.out, &rendered),
            }
        }
        Ok(())
    }
}

/// Paragraphs, breaks and tabs of one cell folded into single spaces.
fn flatten_cell(raw: &str) -> String {
    raw.split(['\n', '\t'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Rows as lines, cells separated by tabs. `limit` bounds the output, since a
/// repeated cell multiplies a few bytes of input.
fn render(rows: &[Vec<Cell>], limit: u64) -> Result<String, ExtractError> {
    let too_large = || ExtractError::TooLarge("extracted text passes the size limit".into());
    let mut out = String::new();
    for row in rows {
        let mut first = true;
        for cell in row {
            for _ in 0..cell.repeat {
                if !first {
                    out.push('\t');
                }
                first = false;
                out.push_str(&cell.text);
                if out.len() as u64 > limit {
                    return Err(too_large());
                }
            }
        }
        out.push('\n');
    }
    Ok(out)
}

/// Append a table, set off from the text around it by a blank line on each side.
fn push_block(out: &mut String, block: &str) {
    if !out.is_empty() && !out.ends_with("\n\n") {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(block);
    out.push('\n');
}

/// Depth at which to start skipping because `e` (a `name` element at `depth`)
/// marks an `AttrSkip` scope, if it does. With `opens` the element is a start
/// tag and is recorded in `scopes` if it can be a scope; an empty tag cannot
/// contain anything, so only its marker role counts.
fn attr_skip(
    rules: &Rules,
    e: &BytesStart<'_>,
    name: &str,
    depth: usize,
    scopes: &mut Vec<(usize, &'static str)>,
    opens: bool,
) -> Option<usize> {
    if opens {
        for rule in rules.skip_by_attr {
            if rule.scope == name {
                scopes.push((depth, rule.scope));
            }
        }
    }
    for rule in rules.skip_by_attr.iter().filter(|rule| rule.marker == name) {
        if attribute(e, rule.attr).is_some_and(|value| rule.values.contains(&value.as_str())) {
            if let Some((scope_depth, _)) = scopes.iter().rev().find(|(_, scope)| *scope == rule.scope) {
                return Some(*scope_depth);
            }
        }
    }
    None
}

/// Forget the scopes opened at `depth` or deeper, as the element at `depth`
/// closes. Scopes are pushed in depth order, so this only pops: nesting a
/// million `footnote` elements must not make every close scan them all.
fn close_scopes(scopes: &mut Vec<(usize, &'static str)>, depth: usize) {
    while scopes.last().is_some_and(|(scope_depth, _)| *scope_depth >= depth) {
        scopes.pop();
    }
}

/// The text of one XML part under `rules`: paragraphs one per line, tables as
/// tab-separated rows. `limit` is how many bytes of text may still be produced.
fn xml_text(xml: &str, rules: &Rules, limit: u64) -> Result<String, ExtractError> {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::default();
    let mut inside = 0usize;
    // Open elements, the depth whose subtree is being skipped, and the open
    // elements that an `AttrSkip` may still decide to drop.
    let mut depth = 0usize;
    let mut skip_from: Option<usize> = None;
    let mut scopes: Vec<(usize, &'static str)> = Vec::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|e| ExtractError::Malformed(format!("XML at byte {}: {e}", reader.error_position())))?;
        if let Some(from) = skip_from {
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    if depth == from {
                        skip_from = None;
                    }
                    close_scopes(&mut scopes, depth);
                    depth = depth.saturating_sub(1);
                }
                Event::Eof => break,
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(e) => {
                depth += 1;
                let name = e.local_name();
                let name = name.as_ref();
                if rules.skip_elements.contains(&name) {
                    skip_from = Some(depth);
                    continue;
                }
                if let Some(from) = attr_skip(rules, &e, name, depth, &mut scopes, true) {
                    skip_from = Some(from);
                    continue;
                }
                if rules.text.contains(&name) {
                    inside += 1;
                }
                if let Some(table) = &rules.table {
                    open_table_element(&mut writer, table, &e, name);
                }
            }
            Event::End(e) => {
                let name = e.local_name();
                let name = name.as_ref();
                if rules.text.contains(&name) {
                    inside = inside.saturating_sub(1);
                }
                if let Some(table) = &rules.table {
                    writer.close(table, name, limit)?;
                }
                if rules.paragraph.contains(&name) {
                    writer.sink().push('\n');
                }
                close_scopes(&mut scopes, depth);
                depth = depth.saturating_sub(1);
            }
            Event::Empty(e) => {
                let name = e.local_name();
                let name = name.as_ref();
                if let Some(from) = attr_skip(rules, &e, name, depth + 1, &mut scopes, false) {
                    skip_from = Some(from);
                    continue;
                }
                if let Some(table) = &rules.table {
                    open_table_element(&mut writer, table, &e, name);
                    if table.cells.contains(&name) {
                        writer.close(table, name, limit)?;
                    }
                }
                push_empty(writer.sink(), &e, rules);
            }
            Event::Text(t) if inside > 0 => writer.sink().push_str(&t.xml10_content()),
            Event::CData(t) if inside > 0 => writer.sink().push_str(&t.xml10_content()),
            Event::GeneralRef(r) if inside > 0 => push_reference(writer.sink(), &r),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(writer.out.trim_end().to_string())
}

/// Table bookkeeping for a start or empty tag `e` named `name`: open the table,
/// row or cell it is, and note a Word cell's `gridSpan`. Called by `xml_text`.
fn open_table_element(writer: &mut Writer, table: &TableRules, e: &BytesStart<'_>, name: &str) {
    if table.grid_span == Some(name) {
        let span = attribute(e, "val").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1);
        writer.span(span);
        return;
    }
    let repeat = table
        .repeat
        .and_then(|attr| attribute(e, attr))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, MAX_CELL_REPEAT);
    writer.open(table, name, repeat);
}

/// Self-closing structure: tabs, breaks, ODF spaces and empty paragraphs.
fn push_empty(out: &mut String, e: &BytesStart<'_>, rules: &Rules) {
    let name = e.local_name();
    let name = name.as_ref();
    if rules.tab.contains(&name) {
        out.push('\t');
    } else if rules.line_break.contains(&name) || rules.paragraph.contains(&name) {
        out.push('\n');
    } else if rules.spaces == Some(name) {
        let count = attribute(e, "c").and_then(|c| c.parse::<usize>().ok()).unwrap_or(1);
        // `text:c` is attacker-controlled; a page of spaces is plenty.
        out.extend(std::iter::repeat_n(' ', count.min(4096)));
    }
}

/// `&amp;`, `&#38;` and the like arrive as separate events since quick-xml 0.38.
fn push_reference(out: &mut String, r: &quick_xml::events::BytesRef<'_>) {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        out.push(c);
    } else if let Some(s) = quick_xml::escape::resolve_predefined_entity(r) {
        out.push_str(s);
    }
}

fn attribute(e: &BytesStart<'_>, local: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == local)
        .map(|a| quick_xml::escape::unescape(&a.value).map(|v| v.into_owned()).unwrap_or_else(|_| a.value.into_owned()))
}

/// The `r:id` of a `sldId`: the attribute named `id` that has a namespace
/// prefix, since the same element also carries a plain numeric `id`.
fn relationship_id(e: &BytesStart<'_>) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.prefix().is_some() && a.key.local_name().as_ref() == "id")
        .map(|a| quick_xml::escape::unescape(&a.value).map(|v| v.into_owned()).unwrap_or_else(|_| a.value.into_owned()))
}

/// ODP keeps every slide in content.xml as a `draw:page`; split on those. A
/// slide's notes (`presentation:notes`) become their own `Slide N notes`
/// section. A page name the editor generated (`page1`) is shown as `Slide N`;
/// a name the author gave is kept.
fn odp_slides(package: &mut Package<'_>, xml: &str) -> Result<Vec<Section>, ExtractError> {
    let mut sections = Vec::new();
    let mut rest = xml;
    let mut number = 0;
    while let Some(range) = element_range(rest, "draw:page") {
        let page = &rest[range.clone()];
        number += 1;
        let name = Reader::from_str(page)
            .read_event()
            .ok()
            .and_then(|event| match event {
                Event::Start(e) | Event::Empty(e) => attribute(&e, "name"),
                _ => None,
            })
            .filter(|name| !is_generated_page_name(name))
            .unwrap_or_else(|| format!("Slide {number}"));
        let notes = element_range(page, "presentation:notes");
        let slide_xml = match &notes {
            Some(notes) => Cow::Owned([&page[..notes.start], &page[notes.end..]].concat()),
            None => Cow::Borrowed(page),
        };
        sections.push(Section {
            kind: SectionKind::Slide,
            name: Some(name.clone()),
            text: package.text("content.xml", &slide_xml, &ODP)?,
        });
        if let Some(notes) = notes {
            let text = package.text("content.xml", &page[notes], &ODP)?;
            if !text.is_empty() {
                sections.push(Section { kind: SectionKind::Notes, name: Some(format!("{name} notes")), text });
            }
        }
        rest = &rest[range.end..];
    }
    Ok(sections)
}

/// `page` followed by digits: the name LibreOffice and similar editors give a
/// slide the author did not name.
fn is_generated_page_name(name: &str) -> bool {
    name.strip_prefix("page")
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Byte range of the next `<tag ...>...</tag>` in `xml` (or of a self-closing
/// `<tag/>`), or up to the end when the close tag is missing.
fn element_range(xml: &str, tag: &str) -> Option<Range<usize>> {
    let start = find_tag(xml, tag)?;
    let after = &xml[start..];
    let mut reader = Reader::from_str(after);
    let empty = match reader.read_event() {
        Ok(Event::Empty(_)) => true,
        Ok(Event::Start(_)) => false,
        _ => return Some(start..xml.len()),
    };
    let open_len = reader.buffer_position() as usize;
    if empty {
        return Some(start..start + open_len);
    }
    let close = format!("</{tag}>");
    let end = after[open_len..]
        .find(&close)
        .map_or(after.len(), |i| open_len + i + close.len());
    Some(start..start + end)
}

/// Byte offset of the next `<tag` that is the whole element name.
fn find_tag(xml: &str, tag: &str) -> Option<usize> {
    let open = format!("<{tag}");
    let mut from = 0;
    while let Some(i) = xml[from..].find(&open) {
        let at = from + i;
        match xml[at + open.len()..].chars().next() {
            Some(c) if c.is_whitespace() || c == '>' || c == '/' => return Some(at),
            _ => from = at + open.len(),
        }
    }
    None
}
