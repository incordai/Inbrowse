//! PDF text via pdf-extract, one section per page. Called by
//! `extract_with_limits` (lib.rs), which also catches pdf-extract's panics.
//!
//! pdf-extract's own `extract_text_from_mem_by_pages` stops at the first page
//! it cannot read and returns the pages before it as if they were the whole
//! file, and it has no way to say a page is an image. So the pages are read one
//! at a time here: a page that fails is reported in the warnings and the pages
//! after it are still read. lopdf is reached through pdf-extract's re-export,
//! so the PDF parser stays at the one version pdf-extract was built against.

use std::panic::{catch_unwind, AssertUnwindSafe};

use pdf_extract::{Document, Error, LoadOptions, Object, ObjectId, PlainTextOutput};

use crate::{ExtractError, Section, SectionKind};

/// How many form XObjects deep an image is looked for on a page.
const MAX_FORM_DEPTH: usize = 4;
/// How many `/Parent` links are followed when looking up inherited resources.
const MAX_PAGE_TREE_DEPTH: usize = 32;

/// One section per page. `password` opens an encrypted file; the empty user
/// password is always tried first, as readers do for owner-only protection.
/// Pages that fail and pages that are probably scans are added to `warnings`.
pub(crate) fn extract(
    bytes: &[u8],
    password: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<Vec<Section>, ExtractError> {
    let doc = load(bytes, password)?;
    let mut sections = Vec::new();
    for (number, id) in doc.get_pages() {
        let text = match page_text(&doc, number) {
            Ok(text) => {
                let text = text.trim().to_string();
                if text.is_empty() && page_has_images(&doc, id) {
                    warnings.push(format!(
                        "no_text_layer: page {number} has no text layer; it may be scanned"
                    ));
                }
                text
            }
            Err(why) => {
                warnings.push(format!(
                    "page_failed: page {number} could not be read ({why}); its text is missing"
                ));
                String::new()
            }
        };
        sections.push(Section {
            kind: SectionKind::Page,
            name: Some(format!("Page {number}")),
            text,
        });
    }
    Ok(sections)
}

/// Parse `bytes`, decrypting with `password` when the file is encrypted. lopdf
/// decrypts while loading (it tries the empty user password first, then
/// `password`); a file it could not open comes back with its objects missing
/// and its encryption dictionary still in place, which is how a missing
/// password shows. (pdf-extract's `_encrypted` functions decrypt after loading,
/// which finds nothing to decrypt and returns no pages.) A wrong or missing
/// password is [`ExtractError::Encrypted`] with text that says which.
fn load(bytes: &[u8], password: Option<&str>) -> Result<Document, ExtractError> {
    let options = LoadOptions {
        password: password.map(String::from),
        ..LoadOptions::default()
    };
    let doc = Document::load_mem_with_options(bytes, options).map_err(|e| match e {
        Error::InvalidPassword => ExtractError::Encrypted("the password is incorrect".into()),
        other => ExtractError::Malformed(format!("PDF: {other}")),
    })?;
    if doc.is_encrypted() {
        return Err(ExtractError::Encrypted(
            "the PDF is password-protected and needs a password".into(),
        ));
    }
    Ok(doc)
}

/// The text of page `number`. pdf-extract unwraps on pages it cannot place
/// (no MediaBox, no content), so a panic is turned into the error text here
/// rather than losing the whole document.
fn page_text(doc: &Document, number: u32) -> Result<String, String> {
    let read = catch_unwind(AssertUnwindSafe(|| {
        let mut text = String::new();
        let mut output = PlainTextOutput::new(&mut text);
        pdf_extract::output_doc_page(doc, &mut output, number).map(|()| text)
    }));
    match read {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("the parser failed on this page".into()),
    }
}

/// Whether the page draws an image XObject, directly or through a form: what
/// a scanner leaves on a page that has no text layer. Resources are inherited
/// from the page tree when the page has none of its own. Called by `extract`;
/// sibling of `resources_have_images`.
fn page_has_images(doc: &Document, page: ObjectId) -> bool {
    let mut node = doc.get_dictionary(page).ok();
    for _ in 0..MAX_PAGE_TREE_DEPTH {
        let Some(dict) = node else { return false };
        if let Ok(resources) = dict.get(b"Resources") {
            return resources_have_images(doc, resources, 0);
        }
        node = dict
            .get_deref(b"Parent", doc)
            .and_then(Object::as_dict)
            .ok();
    }
    false
}

/// Whether the resource dictionary `resources` (possibly a reference) lists an
/// image XObject, or a form XObject whose own resources do, up to
/// `MAX_FORM_DEPTH` forms deep. Helper of `page_has_images`.
fn resources_have_images(doc: &Document, resources: &Object, depth: usize) -> bool {
    let Ok((_, resources)) = doc.dereference(resources) else {
        return false;
    };
    let Ok(xobjects) = resources
        .as_dict()
        .and_then(|dict| dict.get_deref(b"XObject", doc))
        .and_then(Object::as_dict)
    else {
        return false;
    };
    xobjects.iter().any(|(_, xobject)| {
        let Ok(stream) = doc
            .dereference(xobject)
            .and_then(|(_, object)| object.as_stream())
        else {
            return false;
        };
        match stream.dict.get(b"Subtype").and_then(Object::as_name) {
            Ok(b"Image") => true,
            Ok(b"Form") => {
                depth < MAX_FORM_DEPTH
                    && stream
                        .dict
                        .get(b"Resources")
                        .is_ok_and(|inner| resources_have_images(doc, inner, depth + 1))
            }
            _ => false,
        }
    })
}
