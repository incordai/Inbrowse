//! Recognise a document from its bytes. Called by `extract_with_limits` in
//! lib.rs; opens archives through `office::open_archive` so the entry-count
//! limit applies here too.

use std::io::Read;

use crate::{office, ExtractError, Format, Limits};

const ZIP_MAGIC: &[u8] = b"PK\x03\x04";
const OLE_MAGIC: &[u8] = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";
/// The PDF header may follow up to 1024 bytes of junk (ISO 32000-1, 7.5.2 note).
const PDF_HEADER_WINDOW: usize = 1024;

/// Identify `bytes`. `hint` (file name, extension or MIME type) only decides
/// whether plain text is CSV.
pub fn detect(bytes: &[u8], hint: Option<&str>, limits: &Limits) -> Result<Format, ExtractError> {
    let head = &bytes[..bytes.len().min(PDF_HEADER_WINDOW)];
    if head.windows(5).any(|w| w == b"%PDF-") {
        return Ok(Format::Pdf);
    }
    if bytes.starts_with(ZIP_MAGIC) {
        return detect_archive(bytes, limits);
    }
    if bytes.starts_with(OLE_MAGIC) {
        // Legacy binary Office. calamine reads .xls; .doc and .ppt are refused
        // there with a clear error rather than guessed at here.
        return Ok(Format::Xls);
    }
    let sample = &bytes[..bytes.len().min(8192)];
    if sample.contains(&0) || std::str::from_utf8(utf8_prefix(sample)).is_err() {
        return Err(ExtractError::Unsupported("binary data in no recognised format".into()));
    }
    let is_csv = hint.is_some_and(|h| {
        let h = h.trim().to_ascii_lowercase();
        h.ends_with(".csv") || h == "csv" || h.starts_with("text/csv")
    });
    Ok(if is_csv { Format::Csv } else { Format::Text })
}

fn detect_archive(bytes: &[u8], limits: &Limits) -> Result<Format, ExtractError> {
    let mut archive = office::open_archive(bytes, limits)?;
    let has = |archive: &zip::ZipArchive<_>, name: &str| archive.index_for_name(name).is_some();
    if has(&archive, "word/document.xml") {
        return Ok(Format::Docx);
    }
    if has(&archive, "ppt/presentation.xml") {
        return Ok(Format::Pptx);
    }
    if has(&archive, "xl/workbook.xml") {
        return Ok(Format::Xlsx);
    }
    if has(&archive, "xl/workbook.bin") {
        return Ok(Format::Xlsb);
    }
    // OpenDocument stores its MIME type, uncompressed, in a `mimetype` entry.
    let mut mime = String::new();
    if let Ok(entry) = archive.by_name("mimetype") {
        entry
            .take(256)
            .read_to_string(&mut mime)
            .map_err(|e| ExtractError::Malformed(format!("mimetype entry: {e}")))?;
    }
    match mime.trim() {
        "application/vnd.oasis.opendocument.text" => Ok(Format::Odt),
        "application/vnd.oasis.opendocument.presentation" => Ok(Format::Odp),
        "application/vnd.oasis.opendocument.spreadsheet" => Ok(Format::Ods),
        _ => Err(ExtractError::Unsupported("zip archive that is not an Office or OpenDocument file".into())),
    }
}

/// Drop a UTF-8 sequence cut off by the end of the sample.
fn utf8_prefix(sample: &[u8]) -> &[u8] {
    match std::str::from_utf8(sample) {
        Err(e) if e.error_len().is_none() => &sample[..e.valid_up_to()],
        _ => sample,
    }
}
