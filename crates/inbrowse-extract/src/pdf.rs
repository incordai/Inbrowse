//! PDF text via pdf-extract, one section per page. Called by
//! `extract_with_limits` (lib.rs), which also catches pdf-extract's panics.

use crate::{ExtractError, Section, SectionKind};

pub(crate) fn extract(bytes: &[u8]) -> Result<Vec<Section>, ExtractError> {
    let pages = pdf_extract::extract_text_from_mem_by_pages(bytes)
        .map_err(|e| ExtractError::Malformed(format!("PDF: {e}")))?;
    Ok(pages
        .into_iter()
        .enumerate()
        .map(|(index, text)| Section {
            kind: SectionKind::Page,
            name: Some(format!("Page {}", index + 1)),
            text: text.trim().to_string(),
        })
        .collect())
}
