//! Each test builds its document in memory, so no binary fixtures are kept.

use std::io::{Cursor, Write};

use inbrowse_extract::{extract, extract_with_limits, ExtractError, Format, Limits, SectionKind};
use zip::write::SimpleFileOptions;

fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let deflated = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in entries {
        writer.start_file(*name, deflated).unwrap();
        writer.write_all(body.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

fn docx(body: &str) -> Vec<u8> {
    zip_of(&[(
        "word/document.xml",
        &format!(r#"<?xml version="1.0"?><w:document {W}><w:body>{body}</w:body></w:document>"#),
    )])
}

#[test]
fn docx_keeps_paragraphs_tabs_breaks_and_entities() {
    let bytes = docx(
        r#"<w:p><w:r><w:t>Fish &amp; chips</w:t><w:tab/><w:t xml:space="preserve"> &#169; 2026</w:t></w:r></w:p>
           <w:p><w:r><w:t>line one</w:t><w:br/><w:t>line two</w:t></w:r></w:p>
           <w:p><w:r><w:instrText>PAGE</w:instrText></w:r></w:p>"#,
    );
    let doc = extract(&bytes, None).unwrap();
    assert_eq!(doc.format, Format::Docx);
    // Field codes such as PAGE are markup, not document text.
    assert_eq!(doc.text, "Fish & chips\t \u{a9} 2026\nline one\nline two");
}

#[test]
fn pptx_orders_slides_numerically() {
    let slide = |text: &str| {
        format!(r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:cSld></p:sld>"#)
    };
    let (two, ten) = (slide("second"), slide("tenth"));
    let bytes = zip_of(&[
        ("ppt/presentation.xml", "<p:presentation xmlns:p=\"p\"/>"),
        ("ppt/slides/slide10.xml", &ten),
        ("ppt/slides/slide2.xml", &two),
    ]);
    let doc = extract(&bytes, None).unwrap();
    assert_eq!(doc.format, Format::Pptx);
    let names: Vec<_> = doc.sections.iter().map(|s| s.name.as_deref().unwrap()).collect();
    assert_eq!(names, ["Slide 2", "Slide 10"]);
    assert!(doc.sections.iter().all(|s| s.kind == SectionKind::Slide));
    assert_eq!(doc.text, "second\n\ntenth");
}

#[test]
fn odt_expands_spaces_and_reads_headings_and_spans() {
    let content = r#"<office:document-content xmlns:office="o" xmlns:text="t"><office:body><office:text>
        <text:h>Title</text:h>
        <text:p>a<text:s text:c="3"/>b<text:tab/><text:span>c</text:span><text:line-break/>d</text:p>
        </office:text></office:body></office:document-content>"#;
    let bytes = zip_of(&[("mimetype", "application/vnd.oasis.opendocument.text"), ("content.xml", content)]);
    let doc = extract(&bytes, None).unwrap();
    assert_eq!(doc.format, Format::Odt);
    assert_eq!(doc.text, "Title\na   b\tc\nd");
}

#[test]
fn odp_splits_on_draw_page_and_keeps_names() {
    let content = r#"<office:document-content xmlns:office="o" xmlns:text="t" xmlns:draw="d"><office:body><office:presentation>
        <draw:page draw:name="Intro &amp; goals"><draw:frame><draw:text-box><text:p>one</text:p></draw:text-box></draw:frame></draw:page>
        <draw:page><draw:frame><draw:text-box><text:p>two</text:p></draw:text-box></draw:frame></draw:page>
        </office:presentation></office:body></office:document-content>"#;
    let bytes = zip_of(&[("mimetype", "application/vnd.oasis.opendocument.presentation"), ("content.xml", content)]);
    let doc = extract(&bytes, None).unwrap();
    assert_eq!(doc.format, Format::Odp);
    let got: Vec<_> = doc.sections.iter().map(|s| (s.name.clone().unwrap(), s.text.clone())).collect();
    assert_eq!(got, [("Intro & goals".to_string(), "one".to_string()), ("Slide 2".to_string(), "two".to_string())]);
}

#[test]
fn xlsx_reads_each_sheet_as_tab_separated_rows() {
    let bytes = zip_of(&[
        (
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
        ),
        (
            "xl/workbook.xml",
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Prices" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>item</t></is></c><c r="B1" t="inlineStr"><is><t>price</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>tea</t></is></c><c r="B2"><v>2.5</v></c></row></sheetData></worksheet>"#,
        ),
    ]);
    let doc = extract(&bytes, None).unwrap();
    assert_eq!(doc.format, Format::Xlsx);
    assert_eq!(doc.sections.len(), 1);
    assert_eq!(doc.sections[0].name.as_deref(), Some("Prices"));
    assert_eq!(doc.sections[0].text, "item\tprice\ntea\t2.5");
}

/// A one-page PDF with correct xref offsets, showing "Hello PDF" in Helvetica.
fn minimal_pdf() -> Vec<u8> {
    let stream = "BT /F1 24 Tf 72 720 Td (Hello PDF) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
    ];
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
    }
    let xref = pdf.len();
    pdf.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
    for offset in offsets {
        pdf.push_str(&format!("{offset:010} 00000 n \n"));
    }
    pdf.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1));
    pdf.into_bytes()
}

#[test]
fn pdf_text_is_returned_per_page() {
    let doc = extract(&minimal_pdf(), None).unwrap();
    assert_eq!(doc.format, Format::Pdf);
    assert_eq!(doc.sections.len(), 1);
    assert_eq!(doc.sections[0].name.as_deref(), Some("Page 1"));
    assert!(doc.sections[0].text.contains("Hello PDF"), "{:?}", doc.sections[0].text);
}

#[test]
fn corrupt_pdf_is_an_error_not_a_crash() {
    let mut bytes = minimal_pdf();
    bytes.truncate(bytes.len() / 2);
    let err = extract(&bytes, None).unwrap_err();
    assert!(matches!(err, ExtractError::Malformed(_) | ExtractError::Panicked), "{err:?}");
}

#[test]
fn hint_marks_csv_and_bom_is_removed() {
    let bytes = b"\xEF\xBB\xBFa,b\n1,2\n";
    assert_eq!(extract(bytes, Some("prices.csv")).unwrap().format, Format::Csv);
    let doc = extract(bytes, Some("text/plain")).unwrap();
    assert_eq!(doc.format, Format::Text);
    assert_eq!(doc.text, "a,b\n1,2");
}

#[test]
fn unknown_binary_and_plain_zip_are_refused() {
    assert!(matches!(extract(&[0u8, 1, 2, 3, 0xff], None), Err(ExtractError::Unsupported(_))));
    let plain = zip_of(&[("readme.txt", "hello")]);
    assert!(matches!(extract(&plain, None), Err(ExtractError::Unsupported(_))));
}

#[test]
fn archive_that_unpacks_past_the_limit_is_refused() {
    // 4 MiB of one character compresses to a few KiB.
    let bytes = docx(&format!("<w:p><w:r><w:t>{}</w:t></w:r></w:p>", "a".repeat(4 << 20)));
    assert!(bytes.len() < 64 << 10, "fixture should be small: {}", bytes.len());
    let limits = Limits { max_unpacked_bytes: 1 << 20, ..Limits::default() };
    assert!(matches!(extract_with_limits(&bytes, None, &limits), Err(ExtractError::TooLarge(_))));
    assert!(extract(&bytes, None).is_ok(), "the default limit allows it");
}

#[test]
fn entry_count_and_input_size_limits_apply() {
    let bytes = docx("<w:p/>");
    let few_entries = Limits { max_archive_entries: 0, ..Limits::default() };
    assert!(matches!(extract_with_limits(&bytes, None, &few_entries), Err(ExtractError::TooLarge(_))));
    let small_input = Limits { max_input_bytes: 10, ..Limits::default() };
    assert!(matches!(extract_with_limits(&bytes, None, &small_input), Err(ExtractError::TooLarge(_))));
}

#[test]
fn json_shape_is_stable() {
    let doc = extract(&docx("<w:p><w:r><w:t>hi</w:t></w:r></w:p>"), None).unwrap();
    assert_eq!(
        serde_json::to_value(&doc).unwrap(),
        serde_json::json!({"format": "docx", "text": "hi", "sections": [{"kind": "body", "text": "hi"}]})
    );
}
