//! Spreadsheet text via calamine: one section per sheet, cells separated by
//! tabs and rows by newlines, dates as ISO 8601. Called by
//! `extract_with_limits` (lib.rs).

use std::io::{Cursor, Read, Seek};

use calamine::{Data, ExcelDateTime, Range, Reader};

use crate::{office, ExtractError, Format, Limits, Section, SectionKind};

/// Milliseconds in a day, the unit an Excel serial's fraction is read in.
const DAY_MS: f64 = 86_400_000.0;

pub(crate) fn extract(
    bytes: &[u8],
    format: Format,
    limits: &Limits,
    warnings: &mut Vec<String>,
) -> Result<Vec<Section>, ExtractError> {
    if format != Format::Xls {
        check_declared_size(bytes, limits)?;
    }
    // detect.rs already knows the format, so open that reader directly:
    // calamine's auto-detection hides every reader's own error behind
    // "Cannot detect file format".
    let cursor = Cursor::new(bytes);
    match format {
        Format::Xlsx => sheets(open::<calamine::Xlsx<_>, _>(cursor)?, warnings),
        Format::Xlsb => sheets(open::<calamine::Xlsb<_>, _>(cursor)?, warnings),
        Format::Xls => sheets(open::<calamine::Xls<_>, _>(cursor)?, warnings),
        Format::Ods => sheets(open::<calamine::Ods<_>, _>(cursor)?, warnings),
        other => unreachable!("sheet::extract called for {other:?}"),
    }
}

fn open<R, RS>(reader: RS) -> Result<R, ExtractError>
where
    RS: Read + Seek,
    R: Reader<RS>,
    R::Error: std::fmt::Display,
{
    calamine::open_workbook_from_rs(reader).map_err(|e| ExtractError::Malformed(format!("spreadsheet: {e}")))
}

fn sheets<R, RS>(mut workbook: R, warnings: &mut Vec<String>) -> Result<Vec<Section>, ExtractError>
where
    RS: Read + Seek,
    R: Reader<RS>,
    R::Error: std::fmt::Display,
{
    let names = workbook.sheet_names();
    let mut sections = Vec::with_capacity(names.len());
    for name in names {
        let range = workbook
            .worksheet_range(&name)
            .map_err(|e| ExtractError::Malformed(format!("sheet {name:?}: {e}")))?;
        match workbook.worksheet_formula(&name) {
            Ok(formulas) => {
                let missing = formulas_without_value(&formulas, &range);
                if missing > 0 {
                    let cells = if missing == 1 { "cell has" } else { "cells have" };
                    warnings.push(format!("no_cached_value: sheet {name:?}: {missing} formula {cells} no cached value"));
                }
            }
            Err(e) => warnings.push(format!("formulas_unreadable: sheet {name:?}: {e}")),
        }
        let text = range
            .rows()
            .map(|row| {
                let cells: Vec<String> = row.iter().map(cell_text).collect();
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

/// How many cells hold a formula but no value in `values`: files written by a
/// library that never calculated them (the cell is empty until a spreadsheet
/// program opens and saves the file). Positions in both ranges are absolute,
/// and the two ranges need not start at the same cell. Called by `sheets`.
fn formulas_without_value(formulas: &Range<String>, values: &Range<Data>) -> usize {
    let Some((first_row, first_col)) = formulas.start() else { return 0 };
    formulas
        .used_cells()
        .filter(|(row, col, _)| {
            let at = (first_row + *row as u32, first_col + *col as u32);
            values.get_value(at).is_none_or(|value| matches!(value, Data::Empty))
        })
        .count()
}

/// One cell as text. Spreadsheet date serials print as ISO 8601 so a reader
/// does not have to know the workbook's date system or number formats (ods
/// files already store dates that way); everything else prints as calamine
/// does.
fn cell_text(cell: &Data) -> String {
    match cell {
        Data::DateTime(value) if value.is_duration() => duration_text(value),
        Data::DateTime(value) => datetime_text(value),
        other => other.to_string(),
    }
}

/// `YYYY-MM-DD` for a serial with no time of day, `YYYY-MM-DDTHH:MM:SS` (plus
/// `.mmm` when there are milliseconds) with one, and `HH:MM:SS` for a bare time
/// (a serial under one day, which is how a time-only cell is stored). The
/// serial is read in the workbook's own date system.
fn datetime_text(value: &ExcelDateTime) -> String {
    let (year, month, day, hour, minute, second, milli) = value.to_ymd_hms_milli();
    let time = if milli == 0 {
        format!("{hour:02}:{minute:02}:{second:02}")
    } else {
        format!("{hour:02}:{minute:02}:{second:02}.{milli:03}")
    };
    if value.as_f64().abs() < 1.0 {
        time
    } else if (hour, minute, second, milli) == (0, 0, 0, 0) {
        format!("{year:04}-{month:02}-{day:02}")
    } else {
        format!("{year:04}-{month:02}-{day:02}T{time}")
    }
}

/// An elapsed time (`[h]:mm:ss` formats) as `H:MM:SS`, hours not wrapped at 24,
/// with `.mmm` when there are milliseconds.
fn duration_text(value: &ExcelDateTime) -> String {
    let total = (value.as_f64() * DAY_MS).round() as i64;
    let sign = if total < 0 { "-" } else { "" };
    let total = total.unsigned_abs();
    let (hours, minutes, seconds, milli) = (total / 3_600_000, total / 60_000 % 60, total / 1000 % 60, total % 1000);
    if milli == 0 {
        format!("{sign}{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{sign}{hours}:{minutes:02}:{seconds:02}.{milli:03}")
    }
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
