//! Each test builds its document in memory, so no binary fixtures are kept.

use std::io::{Cursor, Write};

use inbrowse_extract::{detect, extract, extract_with_limits, Document, ExtractError, Format, Limits, Options, SectionKind};
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
            "_rels/.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
        ),
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
    let refused = extract_with_limits(&bytes, None, &limits, &Options::default());
    assert!(matches!(refused, Err(ExtractError::TooLarge(_))));
    assert!(extract(&bytes, None).is_ok(), "the default limit allows it");
}

#[test]
fn entry_count_and_input_size_limits_apply() {
    let bytes = docx("<w:p/>");
    let few_entries = Limits { max_archive_entries: 0, ..Limits::default() };
    let options = Options::default();
    assert!(matches!(extract_with_limits(&bytes, None, &few_entries, &options), Err(ExtractError::TooLarge(_))));
    let small_input = Limits { max_input_bytes: 10, ..Limits::default() };
    assert!(matches!(extract_with_limits(&bytes, None, &small_input, &options), Err(ExtractError::TooLarge(_))));
}

#[test]
fn json_shape_is_stable() {
    let doc = extract(&docx("<w:p><w:r><w:t>hi</w:t></w:r></w:p>"), None).unwrap();
    assert_eq!(
        serde_json::to_value(&doc).unwrap(),
        serde_json::json!({"format": "docx", "text": "hi", "sections": [{"kind": "body", "text": "hi"}], "warnings": []})
    );
}

fn sections_of(doc: &Document) -> Vec<(SectionKind, Option<&str>, &str)> {
    doc.sections.iter().map(|s| (s.kind, s.name.as_deref(), s.text.as_str())).collect()
}

const RELS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// A `.rels` part listing `(id, relationship type suffix, target)`.
fn rels(items: &[(&str, &str, &str)]) -> String {
    let list: String = items
        .iter()
        .map(|(id, kind, target)| format!(r#"<Relationship Id="{id}" Type="{RELS}/{kind}" Target="{target}"/>"#))
        .collect();
    format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{list}</Relationships>"#)
}

fn para(text: &str) -> String {
    format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
}

#[test]
fn docx_reads_headers_footers_notes_and_comments_as_separate_sections() {
    let part = |inner: &str| format!(r#"<w:hdr {W}>{inner}</w:hdr>"#);
    let notes = format!(
        r#"<w:footnotes {W}><w:footnote w:type="separator" w:id="-1">{}</w:footnote><w:footnote w:id="1">{}</w:footnote></w:footnotes>"#,
        para("SEPARATOR"),
        para("FOOTNOTE ONE"),
    );
    let bytes = zip_of(&[
        ("word/document.xml", &format!(r#"<w:document {W}><w:body>{}</w:body></w:document>"#, para("BODYTEXT"))),
        (
            "word/_rels/document.xml.rels",
            &rels(&[
                ("rId1", "header", "header1.xml"),
                ("rId2", "header", "header2.xml"),
                ("rId3", "footer", "footer1.xml"),
                ("rId4", "footnotes", "footnotes.xml"),
                ("rId5", "comments", "/word/comments.xml"),
                ("rId6", "styles", "styles.xml"),
            ]),
        ),
        ("word/header1.xml", &part(&para("HEADERTEXT"))),
        // The same header repeats for another section of the document.
        ("word/header2.xml", &part(&para("HEADERTEXT"))),
        ("word/footer1.xml", &part(&para("FOOTERTEXT"))),
        ("word/footnotes.xml", &notes),
        ("word/comments.xml", &format!(r#"<w:comments {W}><w:comment w:id="0">{}</w:comment></w:comments>"#, para("A COMMENT"))),
    ]);
    let doc = extract(&bytes, None).unwrap();
    assert_eq!(
        sections_of(&doc),
        [
            (SectionKind::Body, None, "BODYTEXT"),
            (SectionKind::Header, Some("Header"), "HEADERTEXT"),
            (SectionKind::Footer, Some("Footer"), "FOOTERTEXT"),
            (SectionKind::Footnotes, Some("Footnotes"), "FOOTNOTE ONE"),
            (SectionKind::Comments, Some("Comments"), "A COMMENT"),
        ]
    );
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
}

#[test]
fn docx_numbers_distinct_headers_and_skips_unreadable_parts_with_a_warning() {
    let part = |text: &str| format!(r#"<w:hdr {W}>{}</w:hdr>"#, para(text));
    let bytes = zip_of(&[
        ("word/document.xml", &format!(r#"<w:document {W}><w:body>{}</w:body></w:document>"#, para("BODY"))),
        (
            "word/_rels/document.xml.rels",
            &rels(&[("rId1", "header", "header1.xml"), ("rId2", "header", "header2.xml"), ("rId3", "footer", "missing.xml"), ("rId4", "header", "bad.xml")]),
        ),
        ("word/header1.xml", &part("FIRST")),
        ("word/header2.xml", &part("SECOND")),
        ("word/bad.xml", "<w:hdr><w:p></w:q>"),
    ]);
    let doc = extract(&bytes, None).unwrap();
    assert_eq!(
        sections_of(&doc),
        [
            (SectionKind::Body, None, "BODY"),
            (SectionKind::Header, Some("Header 1"), "FIRST"),
            (SectionKind::Header, Some("Header 2"), "SECOND"),
        ]
    );
    assert_eq!(doc.warnings.len(), 2, "{:?}", doc.warnings);
    assert!(doc.warnings.iter().any(|w| w.starts_with("part_missing: word/missing.xml")), "{:?}", doc.warnings);
    assert!(doc.warnings.iter().any(|w| w.starts_with("part_skipped: word/bad.xml")), "{:?}", doc.warnings);
}

#[test]
fn docx_header_parts_count_against_the_unpack_limit() {
    let big = format!(r#"<w:hdr {W}>{}</w:hdr>"#, para(&"a".repeat(4 << 20)));
    let bytes = zip_of(&[
        ("word/document.xml", &format!(r#"<w:document {W}><w:body>{}</w:body></w:document>"#, para("BODY"))),
        ("word/_rels/document.xml.rels", &rels(&[("rId1", "header", "header1.xml")])),
        ("word/header1.xml", &big),
    ]);
    let limits = Limits { max_unpacked_bytes: 1 << 20, ..Limits::default() };
    let refused = extract_with_limits(&bytes, None, &limits, &Options::default());
    assert!(matches!(refused, Err(ExtractError::TooLarge(_))), "{refused:?}");
}

fn cell(text: &str) -> String {
    format!("<w:tc>{}</w:tc>", para(text))
}

fn row(cells: &[String]) -> String {
    format!("<w:tr>{}</w:tr>", cells.concat())
}

#[test]
fn docx_table_rows_are_tab_separated_lines_set_off_by_blank_lines() {
    let table = format!(
        "<w:tbl>{}{}</w:tbl>",
        row(&[cell("Name"), cell("Qty"), cell("Price")]),
        row(&[cell("Widget"), cell("4"), cell("9.99")]),
    );
    let doc = extract(&docx(&format!("{}{table}{}", para("before"), para("after"))), None).unwrap();
    assert_eq!(doc.text, "before\n\nName\tQty\tPrice\nWidget\t4\t9.99\n\nafter");
}

#[test]
fn docx_table_cells_join_paragraphs_pad_grid_spans_and_flatten_nested_tables() {
    let spanned = r#"<w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>Total</w:t></w:r></w:p></w:tc>"#;
    let two_paragraphs = format!("<w:tc>{}{}</w:tc>", para("first"), para("second"));
    let nested = format!("<w:tc>{}<w:tbl>{}</w:tbl></w:tc>", para("outer"), row(&[cell("in1"), cell("in2")]));
    let table = format!(
        "<w:tbl>{}{}{}{}</w:tbl>",
        row(&[cell("A"), cell(""), cell("C")]),
        row(&[spanned.to_string(), cell("10")]),
        row(&[two_paragraphs, cell("x")]),
        row(&[nested, cell("y")]),
    );
    let doc = extract(&docx(&table), None).unwrap();
    // Empty cells keep their column; a merged cell is followed by an empty one;
    // a table inside a cell is flattened into that cell.
    assert_eq!(doc.text, "A\t\tC\nTotal\t\t10\nfirst second\tx\nouter in1 in2\ty");
}

#[test]
fn deeply_nested_tables_keep_their_text_without_quadratic_work() {
    let depth = 20_000;
    let open = "<w:tbl><w:tr><w:tc>".repeat(depth);
    let close = "</w:tc></w:tr></w:tbl>".repeat(depth);
    let started = std::time::Instant::now();
    let doc = extract(&docx(&format!("{open}{}{close}", para("deep"))), None).unwrap();
    assert_eq!(doc.text, "deep");
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
}

#[test]
fn table_output_beyond_the_size_limit_is_refused() {
    let repeated = r#"<table:table-cell table:number-columns-repeated="1000"/>"#;
    let rows = format!("<table:table-row>{repeated}{repeated}<table:table-cell><text:p>x</text:p></table:table-cell></table:table-row>").repeat(2000);
    let content = format!(
        r#"<office:document-content xmlns:office="o" xmlns:text="t" xmlns:table="b"><office:body><office:text><table:table>{rows}</table:table></office:text></office:body></office:document-content>"#
    );
    let bytes = zip_of(&[("mimetype", "application/vnd.oasis.opendocument.text"), ("content.xml", &content)]);
    assert!(bytes.len() < 64 << 10, "fixture should be small: {}", bytes.len());
    let limits = Limits { max_unpacked_bytes: 1 << 20, ..Limits::default() };
    let refused = extract_with_limits(&bytes, None, &limits, &Options::default());
    assert!(matches!(refused, Err(ExtractError::TooLarge(_))), "{refused:?}");
}

#[test]
fn odt_tables_pad_covered_cells_repeat_cells_and_flatten_nested_tables() {
    let content = r#"<office:document-content xmlns:office="o" xmlns:text="t" xmlns:table="b"><office:body><office:text>
        <text:p>before</text:p>
        <table:table>
          <table:table-row><table:table-cell><text:p>Name</text:p></table:table-cell><table:table-cell><text:p>Qty</text:p></table:table-cell><table:table-cell><text:p>Price</text:p></table:table-cell></table:table-row>
          <table:table-row><table:table-cell table:number-columns-spanned="2"><text:p>Total</text:p></table:table-cell><table:covered-table-cell/><table:table-cell><text:p>9.99</text:p></table:table-cell></table:table-row>
          <table:table-row><table:table-cell table:number-columns-repeated="2"><text:p>r</text:p></table:table-cell><table:table-cell><text:p>one</text:p><text:p>two</text:p></table:table-cell></table:table-row>
          <table:table-row><table:table-cell><table:table><table:table-row><table:table-cell><text:p>in1</text:p></table:table-cell><table:table-cell><text:p>in2</text:p></table:table-cell></table:table-row></table:table></table:table-cell><table:table-cell/></table:table-row>
        </table:table>
        <text:p>after</text:p>
        </office:text></office:body></office:document-content>"#;
    let bytes = zip_of(&[("mimetype", "application/vnd.oasis.opendocument.text"), ("content.xml", content)]);
    let doc = extract(&bytes, None).unwrap();
    assert_eq!(doc.text, "before\n\nName\tQty\tPrice\nTotal\t\t9.99\nr\tr\tone two\nin1 in2\t\n\nafter");
}

const SLIDE_NS: &str = r#"xmlns:p="p" xmlns:a="a" xmlns:r="r""#;

fn slide(shapes: &str) -> String {
    format!(r#"<p:sld {SLIDE_NS}><p:cSld><p:spTree>{shapes}</p:spTree></p:cSld></p:sld>"#)
}

fn shape(placeholder: Option<&str>, text: &str) -> String {
    let ph = placeholder.map(|t| format!(r#"<p:ph type="{t}"/>"#)).unwrap_or_default();
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="s"/><p:cNvSpPr/><p:nvPr>{ph}</p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:txBody></p:sp>"#
    )
}

#[test]
fn pptx_follows_presentation_order_and_reads_notes_without_placeholders() {
    let presentation = format!(
        r#"<p:presentation {SLIDE_NS}><p:sldIdLst><p:sldId id="257" r:id="rId7"/><p:sldId id="256" r:id="rId3"/></p:sldIdLst></p:presentation>"#
    );
    let notes = |text: &str| {
        slide(&format!("{}{}{}", shape(Some("body"), text), shape(Some("sldNum"), "9"), shape(Some("ftr"), "FOOTER")))
    };
    let bytes = zip_of(&[
        ("ppt/presentation.xml", &presentation),
        ("ppt/_rels/presentation.xml.rels", &rels(&[("rId3", "slide", "slides/slide1.xml"), ("rId7", "slide", "slides/slide2.xml")])),
        ("ppt/slides/slide1.xml", &slide(&format!("{}{}", shape(None, "was first in the file"), shape(Some("sldNum"), "1")))),
        ("ppt/slides/slide2.xml", &slide(&shape(Some("title"), "was second"))),
        ("ppt/slides/_rels/slide1.xml.rels", &rels(&[("rId1", "notesSlide", "../notesSlides/notesSlide1.xml")])),
        ("ppt/notesSlides/notesSlide1.xml", &notes("NOTES ONE")),
    ]);
    let doc = extract(&bytes, None).unwrap();
    assert_eq!(
        sections_of(&doc),
        [
            (SectionKind::Slide, Some("Slide 1"), "was second"),
            (SectionKind::Slide, Some("Slide 2"), "was first in the file"),
            (SectionKind::Notes, Some("Slide 2 notes"), "NOTES ONE"),
        ]
    );
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
}

#[test]
fn pptx_tables_keep_empty_and_merged_cells_aligned() {
    let tc = |attrs: &str, text: &str| format!(r#"<a:tc{attrs}><a:txBody><a:p><a:r><a:t>{text}</a:t></a:r></a:p></a:txBody></a:tc>"#);
    let table = format!(
        r#"<p:graphicFrame><a:graphic><a:graphicData><a:tbl><a:tr>{}{}{}</a:tr><a:tr>{}{}{}</a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>"#,
        tc("", "A"),
        tc("", ""),
        tc("", "C"),
        tc(r#" gridSpan="2""#, "Total"),
        tc(r#" hMerge="1""#, ""),
        tc("", "9"),
    );
    let bytes = zip_of(&[("ppt/presentation.xml", "<p:presentation xmlns:p=\"p\"/>"), ("ppt/slides/slide1.xml", &slide(&table))]);
    let doc = extract(&bytes, None).unwrap();
    assert_eq!(doc.text, "A\t\tC\nTotal\t\t9");
}

#[test]
fn pptx_with_a_missing_listed_slide_warns_and_goes_on() {
    let presentation = format!(r#"<p:presentation {SLIDE_NS}><p:sldIdLst><p:sldId id="1" r:id="rId1"/><p:sldId id="2" r:id="rId2"/></p:sldIdLst></p:presentation>"#);
    let bytes = zip_of(&[
        ("ppt/presentation.xml", &presentation),
        ("ppt/_rels/presentation.xml.rels", &rels(&[("rId1", "slide", "slides/slide1.xml"), ("rId2", "slide", "slides/slide2.xml")])),
        ("ppt/slides/slide2.xml", &slide(&shape(None, "kept"))),
    ]);
    let doc = extract(&bytes, None).unwrap();
    // The slide keeps the number it has in the presentation.
    assert_eq!(sections_of(&doc), [(SectionKind::Slide, Some("Slide 2"), "kept")]);
    assert_eq!(doc.warnings.len(), 1, "{:?}", doc.warnings);
    assert!(doc.warnings[0].starts_with("part_missing: ppt/slides/slide1.xml"), "{:?}", doc.warnings);
}

#[test]
fn odp_splits_notes_out_skips_placeholders_and_numbers_generated_names() {
    let content = r#"<office:document-content xmlns:office="o" xmlns:text="t" xmlns:draw="d" xmlns:presentation="p"><office:body><office:presentation>
        <draw:page draw:name="page1"><draw:frame><draw:text-box><text:p>one</text:p></draw:text-box></draw:frame>
          <presentation:notes>
            <draw:page-thumbnail/>
            <draw:frame presentation:class="notes"><draw:text-box><text:p>NOTES1</text:p></draw:text-box></draw:frame>
            <draw:frame presentation:class="page-number"><draw:text-box><text:p><text:page-number>&lt;number&gt;</text:page-number></text:p></draw:text-box></draw:frame>
          </presentation:notes>
        </draw:page>
        <draw:page draw:name="Agenda"><draw:frame><draw:text-box><text:p>two <presentation:placeholder>&lt;footer&gt;</presentation:placeholder>end</text:p></draw:text-box></draw:frame>
          <draw:frame presentation:class="page-number"><draw:text-box><text:p><text:page-number>&lt;number&gt;</text:page-number></text:p></draw:text-box></draw:frame>
        </draw:page>
        </office:presentation></office:body></office:document-content>"#;
    let bytes = zip_of(&[("mimetype", "application/vnd.oasis.opendocument.presentation"), ("content.xml", content)]);
    let doc = extract(&bytes, None).unwrap();
    assert_eq!(
        sections_of(&doc),
        [
            (SectionKind::Slide, Some("Slide 1"), "one"),
            (SectionKind::Notes, Some("Slide 1 notes"), "NOTES1"),
            (SectionKind::Slide, Some("Agenda"), "two end"),
        ]
    );
    assert!(!doc.text.contains("<number>"));
}

/// A version 3 compound file whose directory lists up to three empty streams.
fn cfb_with_streams(streams: &[&str]) -> Vec<u8> {
    const FREE: u32 = 0xFFFF_FFFF;
    const END: u32 = 0xFFFF_FFFE;
    const FAT_SECTOR: u32 = 0xFFFF_FFFD;
    let mut file = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    file.extend([0u8; 16]); // CLSID
    file.extend(0x003Eu16.to_le_bytes()); // minor version
    file.extend(3u16.to_le_bytes()); // major version
    file.extend(0xFFFEu16.to_le_bytes()); // byte order
    file.extend(9u16.to_le_bytes()); // sector shift: 512 bytes
    file.extend(6u16.to_le_bytes()); // mini sector shift
    file.extend([0u8; 6]); // reserved
    file.extend(0u32.to_le_bytes()); // directory sectors (0 in version 3)
    file.extend(1u32.to_le_bytes()); // FAT sectors
    file.extend(1u32.to_le_bytes()); // first directory sector
    file.extend(0u32.to_le_bytes()); // transaction signature
    file.extend(4096u32.to_le_bytes()); // mini stream cutoff
    file.extend(END.to_le_bytes()); // first mini FAT sector
    file.extend(0u32.to_le_bytes()); // mini FAT sectors
    file.extend(END.to_le_bytes()); // first DIFAT sector
    file.extend(0u32.to_le_bytes()); // DIFAT sectors
    file.extend(0u32.to_le_bytes()); // DIFAT[0]: the FAT is sector 0
    file.extend(std::iter::repeat_n(FREE.to_le_bytes(), 108).flatten());
    assert_eq!(file.len(), 512);

    let mut fat = vec![FAT_SECTOR, END];
    fat.resize(128, FREE);
    file.extend(fat.iter().flat_map(|entry| entry.to_le_bytes()));

    let entry = |name: &str, object_type: u8| {
        let mut raw = [0u8; 128];
        for (i, unit) in name.encode_utf16().enumerate() {
            raw[i * 2..i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
        }
        raw[64..66].copy_from_slice(&((name.encode_utf16().count() as u16 + 1) * 2).to_le_bytes());
        raw[66] = object_type;
        raw[68..80].fill(0xFF); // no siblings, no child
        raw[116..120].copy_from_slice(&END.to_le_bytes());
        raw
    };
    let mut directory = vec![entry("Root Entry", 5)];
    directory.extend(streams.iter().map(|name| entry(name, 2)));
    assert!(directory.len() <= 4);
    directory.resize(4, [0u8; 128]);
    file.extend(directory.concat());
    file
}

#[test]
fn legacy_office_containers_are_named_not_mistaken_for_spreadsheets() {
    let refused = |streams: &[&str]| match extract(&cfb_with_streams(streams), None) {
        Err(error) => error.to_string(),
        Ok(doc) => panic!("{streams:?} was read as {:?}", doc.format),
    };
    let word = refused(&["1Table", "WordDocument"]);
    assert!(word.starts_with("unsupported document: ") && word.contains(".doc") && word.contains(".docx"), "{word}");
    let slides = refused(&["Current User", "PowerPoint Document"]);
    assert!(slides.starts_with("unsupported document: ") && slides.contains(".ppt") && slides.contains(".pptx"), "{slides}");
    let encrypted = refused(&["EncryptionInfo", "EncryptedPackage"]);
    assert!(encrypted.starts_with("encrypted document: ") && encrypted.contains("password"), "{encrypted}");
    let other = refused(&["Contents"]);
    assert!(other.starts_with("unsupported document: "), "{other}");
    assert!(!other.contains("Workbook"), "{other}");
}

#[test]
fn compound_file_with_a_workbook_stream_is_still_xls() {
    let limits = Limits::default();
    for stream in ["Workbook", "Book"] {
        assert_eq!(detect(&cfb_with_streams(&[stream]), None, &limits).unwrap(), Format::Xls);
    }
}

#[test]
fn truncated_or_hostile_compound_files_are_errors_not_panics() {
    let good = cfb_with_streams(&["Workbook"]);
    let err = extract(&good[..8], None).unwrap_err();
    assert!(matches!(err, ExtractError::Malformed(_)), "{err:?}");
    // A directory sector that points back at itself must not loop.
    let mut looping = good.clone();
    let fat_entry_of_directory = 512 + 4;
    looping[fat_entry_of_directory..fat_entry_of_directory + 4].copy_from_slice(&1u32.to_le_bytes());
    let err = extract(&looping, None).unwrap_err();
    assert!(matches!(err, ExtractError::Malformed(_) | ExtractError::Unsupported(_)), "{err:?}");
    // A sector size past what the header allows.
    let mut huge_sector = good.clone();
    huge_sector[30..32].copy_from_slice(&31u16.to_le_bytes());
    assert!(matches!(extract(&huge_sector, None), Err(ExtractError::Malformed(_))));
    // Every prefix of a valid file is handled.
    for end in 0..good.len() {
        let _ = extract(&good[..end], None);
    }
}

/// An xlsx with one sheet named `name`, styles from `styles` (cell formats in
/// order) and the 1904 date system when `date1904`.
fn xlsx_of(name: &str, sheet: &str, styles: Option<&str>, date1904: bool) -> Vec<u8> {
    let workbook_pr = if date1904 { r#"<workbookPr date1904="1"/>"# } else { "" };
    let workbook = format!(
        r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{RELS}">{workbook_pr}<sheets><sheet name="{name}" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    let sheet = format!(r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{sheet}</sheetData></worksheet>"#);
    let mut entries = vec![
        (
            "_rels/.rels",
            format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{RELS}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
        ),
        (
            "[Content_Types].xml",
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#.to_string(),
        ),
        ("xl/workbook.xml", workbook),
        ("xl/_rels/workbook.xml.rels", rels(&[("rId1", "worksheet", "worksheets/sheet1.xml")])),
        ("xl/worksheets/sheet1.xml", sheet),
    ];
    if let Some(formats) = styles {
        let xfs: String = formats.split(',').map(|id| format!(r#"<xf numFmtId="{id}"/>"#)).collect();
        entries.push((
            "xl/styles.xml",
            format!(r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cellXfs count="1">{xfs}</cellXfs></styleSheet>"#),
        ));
    }
    let borrowed: Vec<(&str, &str)> = entries.iter().map(|(name, body)| (*name, body.as_str())).collect();
    zip_of(&borrowed)
}

#[test]
fn xlsx_dates_and_durations_print_as_iso_text() {
    // Format ids: 0 general, 14 date, 22 date and time, 21 time, 46 [h]:mm:ss.
    let sheet = r#"<row r="1"><c r="A1" s="1"><v>46335</v></c><c r="B1" s="2"><v>46335.5</v></c><c r="C1" s="2"><v>46335.25000116</v></c></row>
        <row r="2"><c r="A2" s="3"><v>0.5</v></c><c r="B2" s="4"><v>1.5</v></c><c r="C2" s="0"><v>46335</v></c></row>"#;
    let doc = extract(&xlsx_of("Dates", sheet, Some("0,14,22,21,46"), false), None).unwrap();
    assert_eq!(doc.sections[0].text, "2026-11-09\t2026-11-09T12:00:00\t2026-11-09T06:00:00.100\n12:00:00\t36:00:00\t46335");
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
}

#[test]
fn xlsx_dates_honour_the_1904_date_system() {
    // 46335 days from 1900 is 44873 days from 1904.
    let sheet = r#"<row r="1"><c r="A1" s="1"><v>44873</v></c></row>"#;
    let doc = extract(&xlsx_of("Dates", sheet, Some("0,14"), true), None).unwrap();
    assert_eq!(doc.sections[0].text, "2026-11-09");
}

#[test]
fn xlsx_formula_cells_without_a_cached_value_are_warned_about_per_sheet() {
    let sheet = r#"<row r="1"><c r="A1"><f>1+1</f></c><c r="B1"><f>2+2</f><v>4</v></c><c r="C1"><v>7</v></c></row>
        <row r="2"><c r="A2"><f>A1*2</f></c></row>"#;
    let doc = extract(&xlsx_of("Sales", sheet, None, false), None).unwrap();
    assert_eq!(doc.sections[0].text, "4\t7");
    assert_eq!(doc.warnings, ["no_cached_value: sheet \"Sales\": 2 formula cells have no cached value"]);

    let one = r#"<row r="1"><c r="A1"><f>1+1</f></c><c r="B1"><v>1</v></c></row>"#;
    let doc = extract(&xlsx_of("S", one, None, false), None).unwrap();
    assert_eq!(doc.warnings, ["no_cached_value: sheet \"S\": 1 formula cell has no cached value"]);
}

/// A PDF whose pages are `(extra page entries, content stream)`, all sharing one
/// font and one 1 x 1 image, with correct xref offsets.
fn pdf_of(pages: &[(&str, &str)]) -> Vec<u8> {
    let first_page = 3;
    let font = first_page + 2 * pages.len();
    let image = font + 1;
    let kids: String = (0..pages.len()).map(|i| format!("{} 0 R ", first_page + 2 * i)).collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", pages.len()),
    ];
    for (i, (entries, stream)) in pages.iter().enumerate() {
        // Only a page that draws the image lists it, as a real file would.
        let xobject = if stream.contains("/Im1") { format!("/XObject << /Im1 {image} 0 R >>") } else { String::new() };
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R {entries} /Contents {} 0 R /Resources << /Font << /F1 {font} 0 R >> {xobject} >> >>",
            first_page + 2 * i + 1
        ));
        objects.push(format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()));
    }
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string());
    objects.push("<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n\u{1}\nendstream".to_string());
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

const MEDIA_BOX: &str = "/MediaBox [0 0 612 792]";
const TEXT_STREAM: &str = "BT /F1 24 Tf 72 720 Td (Hello PDF) Tj ET";
const IMAGE_STREAM: &str = "q 100 0 0 100 10 10 cm /Im1 Do Q";

#[test]
fn pdf_pages_without_a_text_layer_are_flagged_only_when_they_hold_images() {
    let bytes = pdf_of(&[(MEDIA_BOX, TEXT_STREAM), (MEDIA_BOX, IMAGE_STREAM), (MEDIA_BOX, "")]);
    let doc = extract(&bytes, None).unwrap();
    assert_eq!(doc.sections.len(), 3);
    assert!(doc.sections[0].text.contains("Hello PDF"));
    assert_eq!(doc.sections[1].text, "");
    assert_eq!(doc.warnings, ["no_text_layer: page 2 has no text layer; it may be scanned"]);
}

#[test]
fn pdf_page_that_fails_does_not_cost_the_pages_after_it() {
    // Page 2 has no MediaBox anywhere, which the text extractor cannot place.
    let bytes = pdf_of(&[(MEDIA_BOX, TEXT_STREAM), ("", TEXT_STREAM), (MEDIA_BOX, TEXT_STREAM)]);
    let doc = extract(&bytes, None).unwrap();
    assert_eq!(doc.sections.len(), 3);
    assert!(doc.sections[0].text.contains("Hello PDF"));
    assert_eq!(doc.sections[1].text, "");
    assert!(doc.sections[2].text.contains("Hello PDF"), "{:?}", doc.sections[2].text);
    assert_eq!(doc.warnings.len(), 1, "{:?}", doc.warnings);
    assert!(doc.warnings[0].starts_with("page_failed: page 2 "), "{:?}", doc.warnings);
}

/// `minimal_pdf` encrypted with the 128-bit RC4 handler, user password "open".
fn encrypted_pdf() -> Vec<u8> {
    use pdf_extract::encryption::{EncryptionState, EncryptionVersion, Permissions};
    let mut doc = pdf_extract::Document::load_mem(&minimal_pdf()).unwrap();
    // The key is derived from the file identifier, which `minimal_pdf` lacks.
    let id = pdf_extract::Object::string_literal("0123456789abcdef");
    doc.trailer.set("ID", pdf_extract::Object::Array(vec![id.clone(), id]));
    let state = EncryptionState::try_from(EncryptionVersion::V2 {
        document: &doc,
        owner_password: "owner",
        user_password: "open",
        key_length: 128,
        permissions: Permissions::all(),
    })
    .unwrap();
    doc.encrypt(&state).unwrap();
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

#[test]
fn encrypted_pdf_needs_the_right_password() {
    let bytes = encrypted_pdf();
    let needs = extract(&bytes, None).unwrap_err();
    assert!(matches!(needs, ExtractError::Encrypted(_)), "{needs:?}");
    assert_eq!(needs.to_string(), "encrypted document: the PDF is password-protected and needs a password");

    let with = |password: &str| {
        let options = Options { password: Some(password.to_string()) };
        extract_with_limits(&bytes, None, &Limits::default(), &options)
    };
    let wrong = with("nope").unwrap_err();
    assert_eq!(wrong.to_string(), "encrypted document: the password is incorrect");

    let doc = with("open").unwrap();
    assert!(doc.sections[0].text.contains("Hello PDF"), "{:?}", doc.sections[0].text);
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
}

#[test]
fn a_password_for_a_file_that_takes_none_is_reported_not_silently_dropped() {
    let options = Options { password: Some("x".to_string()) };
    let doc = extract_with_limits(&docx("<w:p/>"), None, &Limits::default(), &options).unwrap();
    assert_eq!(doc.warnings, ["password_ignored: only PDF files use a password"]);
    let doc = extract_with_limits(&minimal_pdf(), None, &Limits::default(), &options).unwrap();
    assert!(doc.warnings.is_empty(), "an unencrypted PDF ignores it too, quietly: {:?}", doc.warnings);
}
