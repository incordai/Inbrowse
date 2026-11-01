//! Spreadsheet text via calamine: one section per sheet, cells separated by
//! tabs and rows by newlines. Called by `extract_with_limits` (lib.rs).

use std::io::Cursor;

use calamine::Reader;

use crate::{office, ExtractError, Format, Limits, Section, SectionKind};

pub(crate) fn extract(bytes: &[u8], format: Format, limits: &Limits) -> Result<Vec<Section>, ExtractError> {
    if format != Format::Xls {
        check_declared_size(bytes, limits)?;
    }
    let mut workbook = calamine::open_workbook_auto_from_rs(Cursor::new(bytes))
        .map_err(|e| ExtractError::Malformed(format!("spreadsheet: {e}")))?;
    let names = workbook.sheet_names();
    let mut sections = Vec::with_capacity(names.len());
    for name in names {
        let range = workbook
            .worksheet_range(&name)
            .map_err(|e| ExtractError::Malformed(format!("sheet {name:?}: {e}")))?;
        let text = range
            .rows()
            .map(|row| {
                let cells: Vec<String> = row.iter().map(ToString::to_string).collect();
                cells.join("\t").trim_end_matches('\t').to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(Section {
            kind: SectionKind::Sheet,
            name: Some(name),
            text: text.trim_end().to_string(),
        });
    }
    Ok(sections)
}

/// calamine unpacks the archive itself, beyond the capped reads in
/// office.rs, so refuse an archive whose declared sizes already exceed the
/// limit. A forged smaller size is still bounded by calamine's own parsing.
fn check_declared_size(bytes: &[u8], limits: &Limits) -> Result<(), ExtractError> {
    let mut archive = office::open_archive(bytes, limits)?;
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|e| ExtractError::Malformed(format!("zip entry {index}: {e}")))?;
        total = total.saturating_add(entry.size());
        if total > limits.max_unpacked_bytes {
            return Err(ExtractError::TooLarge("archive unpacks past the size limit".into()));
        }
    }
    Ok(())
}
