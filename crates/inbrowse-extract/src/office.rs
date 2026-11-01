//! DOCX, PPTX, ODT and ODP text, read from the zipped XML with quick-xml.
//! Called by `extract_with_limits` (lib.rs); `open_archive` is also used by
//! detect.rs and sheet.rs so every archive is held to the same [`Limits`].

use std::io::{Cursor, Read};

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::{ExtractError, Format, Limits, Section, SectionKind};

pub(crate) type Archive<'a> = zip::ZipArchive<Cursor<&'a [u8]>>;

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

/// Bytes still allowed out of the archive, shared by every entry read.
struct Budget(u64);

fn read_entry(archive: &mut Archive<'_>, name: &str, budget: &mut Budget) -> Result<String, ExtractError> {
    let entry = archive
        .by_name(name)
        .map_err(|e| ExtractError::Malformed(format!("{name}: {e}")))?;
    // The declared size can lie, so the read itself is capped as well.
    let mut raw = Vec::new();
    entry
        .take(budget.0.saturating_add(1))
        .read_to_end(&mut raw)
        .map_err(|e| ExtractError::Malformed(format!("{name}: {e}")))?;
    let read = raw.len() as u64;
    if read > budget.0 {
        return Err(ExtractError::TooLarge("archive unpacks past the size limit".into()));
    }
    budget.0 -= read;
    String::from_utf8(raw).map_err(|_| ExtractError::Malformed(format!("{name} is not UTF-8")))
}

pub(crate) fn extract(bytes: &[u8], format: Format, limits: &Limits) -> Result<Vec<Section>, ExtractError> {
    let mut archive = open_archive(bytes, limits)?;
    let mut budget = Budget(limits.max_unpacked_bytes);
    match format {
        Format::Docx => {
            let xml = read_entry(&mut archive, "word/document.xml", &mut budget)?;
            Ok(vec![body(xml_text(&xml, &OOXML_WORD)?)])
        }
        Format::Pptx => {
            let mut slides: Vec<(u32, String)> = archive
                .file_names()
                .filter_map(|name| {
                    let name = name.ok()?;
                    let number = name.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse().ok()?;
                    Some((number, name.into_owned()))
                })
                .collect();
            slides.sort_unstable();
            slides
                .into_iter()
                .map(|(number, name)| {
                    let xml = read_entry(&mut archive, &name, &mut budget)?;
                    Ok(Section {
                        kind: SectionKind::Slide,
                        name: Some(format!("Slide {number}")),
                        text: xml_text(&xml, &OOXML_DRAWING)?,
                    })
                })
                .collect()
        }
        Format::Odt => {
            let xml = read_entry(&mut archive, "content.xml", &mut budget)?;
            Ok(vec![body(xml_text(&xml, &ODF)?)])
        }
        Format::Odp => {
            let xml = read_entry(&mut archive, "content.xml", &mut budget)?;
            odp_slides(&xml)
        }
        other => unreachable!("office::extract called for {other:?}"),
    }
}

fn body(text: String) -> Section {
    Section { kind: SectionKind::Body, name: None, text }
}

/// Which elements carry text and structure, by local name.
struct Rules {
    /// Text is kept only inside these elements.
    text: &'static [&'static [u8]],
    /// Each of these ends a line.
    paragraph: &'static [&'static [u8]],
    tab: &'static [&'static [u8]],
    line_break: &'static [&'static [u8]],
    /// ODF `text:s`: `text:c` spaces, default one.
    spaces: Option<&'static [u8]>,
}

const OOXML_WORD: Rules = Rules {
    text: &[b"t"],
    paragraph: &[b"p"],
    tab: &[b"tab"],
    line_break: &[b"br", b"cr"],
    spaces: None,
};

const OOXML_DRAWING: Rules = Rules {
    text: &[b"t"],
    paragraph: &[b"p"],
    tab: &[b"tab"],
    line_break: &[b"br"],
    spaces: None,
};

const ODF: Rules = Rules {
    text: &[b"p", b"h"],
    paragraph: &[b"p", b"h"],
    tab: &[b"tab"],
    line_break: &[b"line-break"],
    spaces: Some(b"s"),
};

fn xml_text(xml: &str, rules: &Rules) -> Result<String, ExtractError> {
    let mut reader = Reader::from_str(xml);
    let mut out = String::new();
    let mut inside = 0usize;
    loop {
        let event = reader
            .read_event()
            .map_err(|e| ExtractError::Malformed(format!("XML at byte {}: {e}", reader.error_position())))?;
        match event {
            Event::Start(e) => {
                if rules.text.contains(&e.local_name().as_ref()) {
                    inside += 1;
                }
            }
            Event::End(e) => {
                let name = e.local_name();
                if rules.text.contains(&name.as_ref()) {
                    inside = inside.saturating_sub(1);
                }
                if rules.paragraph.contains(&name.as_ref()) {
                    out.push('\n');
                }
            }
            Event::Empty(e) => push_empty(&mut out, &e, rules),
            Event::Text(t) if inside > 0 => out.push_str(&t.xml10_content()),
            Event::CData(t) if inside > 0 => out.push_str(&t.xml10_content()),
            Event::GeneralRef(r) if inside > 0 => push_reference(&mut out, &r),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out.trim_end().to_string())
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
        let count = attribute(e, b"c").and_then(|c| c.parse::<usize>().ok()).unwrap_or(1);
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

fn attribute(e: &BytesStart<'_>, local: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == local)
        .map(|a| quick_xml::escape::unescape(&a.value).map(|v| v.into_owned()).unwrap_or_else(|_| a.value.into_owned()))
}

/// ODP keeps every slide in content.xml as a `draw:page`; split on those.
fn odp_slides(xml: &str) -> Result<Vec<Section>, ExtractError> {
    let mut sections = Vec::new();
    let mut rest = xml;
    let mut number = 0;
    while let Some(start) = find_tag(rest, "draw:page") {
        let after = &rest[start..];
        let end = after.find("</draw:page>").map(|i| i + "</draw:page>".len()).unwrap_or(after.len());
        let page = &after[..end];
        number += 1;
        let name = Reader::from_str(page).read_event().ok().and_then(|event| match event {
            Event::Start(e) | Event::Empty(e) => attribute(&e, b"name"),
            _ => None,
        });
        sections.push(Section {
            kind: SectionKind::Slide,
            name: Some(name.unwrap_or_else(|| format!("Slide {number}"))),
            text: xml_text(page, &ODF)?,
        });
        rest = &after[end..];
    }
    Ok(sections)
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
