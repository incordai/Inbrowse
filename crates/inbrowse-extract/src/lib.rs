//! Text extraction for documents Inbrowse downloads: PDF, Word and PowerPoint
//! (OOXML), OpenDocument text and presentations, spreadsheets (xlsx, xlsb,
//! xls, ods), CSV and plain text.
//!
//! Entry point: [`extract`] / [`extract_with_limits`], called by the
//! `inbrowse extract` subcommand (inbrowse-cli/src/main.rs `run_extract`) and
//! the MCP `document_extract` tool (inbrowse-mcp `tool_document_extract`).
//! Formats are read by `pdf.rs` (pdf-extract), `sheet.rs` (calamine) and
//! `office.rs` (zip + quick-xml); `cfb.rs` names legacy OLE containers for
//! `detect.rs`. Inputs are untrusted: every archive read is bounded by
//! [`Limits`], and a panic inside a parser becomes [`ExtractError::Panicked`]
//! instead of unwinding into the caller. Content that could not be read, or
//! that is probably missing, is reported in [`Document::warnings`] rather than
//! dropped silently.

mod cfb;
mod detect;
mod office;
mod pdf;
mod sheet;

use serde::Serialize;

pub use detect::detect;

/// Bounds applied to one extraction.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Largest input accepted, in bytes.
    pub max_input_bytes: usize,
    /// Most bytes read out of an archive, summed over its entries.
    pub max_unpacked_bytes: u64,
    /// Most entries an archive may list.
    pub max_archive_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 100 * 1024 * 1024,
            max_unpacked_bytes: 256 * 1024 * 1024,
            max_archive_entries: 10_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Pdf,
    Docx,
    Pptx,
    Xlsx,
    Xlsb,
    Xls,
    Ods,
    Odt,
    Odp,
    Csv,
    Text,
}

/// Per-call settings that are not size bounds. Passed to
/// [`extract_with_limits`] by the CLI (`--password`) and MCP (`password`).
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Password for an encrypted PDF. Other formats ignore it and say so in
    /// [`Document::warnings`].
    pub password: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SectionKind {
    Page,
    Slide,
    Sheet,
    /// The main text of a document with no pages or slides.
    Body,
    /// Word page headers (`office.rs`), named `Header`, or `Header 1`, `Header 2`
    /// when several differ.
    Header,
    Footer,
    Footnotes,
    Endnotes,
    Comments,
    /// Speaker notes, named `Slide N notes`, right after slide `N`.
    Notes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Section {
    pub kind: SectionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Document {
    pub format: Format,
    /// Every section's text, in order, separated by a blank line.
    pub text: String,
    pub sections: Vec<Section>,
    /// Things that were skipped or are probably missing from `text`, each as
    /// `code: message` (for example `no_text_layer: page 2 has no text layer;
    /// it may be scanned`). Always present; empty when nothing was lost.
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub enum ExtractError {
    /// The input or an archive inside it exceeds a [`Limits`] bound.
    TooLarge(String),
    /// The bytes are not a format this crate reads.
    Unsupported(String),
    /// The file is password-protected and no (correct) password was given.
    Encrypted(String),
    /// The format was recognised but the file could not be read.
    Malformed(String),
    /// A parser panicked on this input.
    Panicked,
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge(why) => write!(f, "document too large: {why}"),
            Self::Unsupported(why) => write!(f, "unsupported document: {why}"),
            Self::Encrypted(why) => write!(f, "encrypted document: {why}"),
            Self::Malformed(why) => write!(f, "could not read document: {why}"),
            Self::Panicked => f.write_str("could not read document: the parser failed on this input"),
        }
    }
}

impl std::error::Error for ExtractError {}

/// Extract text with the default [`Limits`].
///
/// `hint` is a file name, extension or MIME type; it is only needed to tell
/// CSV from other plain text, since every other format is recognised from its
/// bytes.
pub fn extract(bytes: &[u8], hint: Option<&str>) -> Result<Document, ExtractError> {
    extract_with_limits(bytes, hint, &Limits::default(), &Options::default())
}

/// [`extract`] with explicit [`Limits`] and [`Options`].
pub fn extract_with_limits(
    bytes: &[u8],
    hint: Option<&str>,
    limits: &Limits,
    options: &Options,
) -> Result<Document, ExtractError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(ExtractError::TooLarge(format!(
            "{} bytes, limit {}",
            bytes.len(),
            limits.max_input_bytes
        )));
    }
    let format = detect(bytes, hint, limits)?;
    let mut warnings = Vec::new();
    if options.password.is_some() && format != Format::Pdf {
        warnings.push("password_ignored: only PDF files use a password".to_string());
    }
    let sections = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match format {
        Format::Pdf => pdf::extract(bytes, options.password.as_deref(), &mut warnings),
        Format::Docx | Format::Pptx | Format::Odt | Format::Odp => {
            office::extract(bytes, format, limits, &mut warnings)
        }
        Format::Xlsx | Format::Xlsb | Format::Xls | Format::Ods => {
            sheet::extract(bytes, format, limits, &mut warnings)
        }
        Format::Csv | Format::Text => Ok(vec![Section {
            kind: SectionKind::Body,
            name: None,
            text: decode_text(bytes),
        }]),
    }))
    .map_err(|_| ExtractError::Panicked)??;

    let text = sections
        .iter()
        .map(|section| section.text.trim_end())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(Document { format, text, sections, warnings })
}

/// UTF-8 with any byte-order mark removed; invalid sequences are replaced.
/// Trailing whitespace is dropped, as every other format's sections do.
fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    String::from_utf8_lossy(bytes).trim_end().to_string()
}
