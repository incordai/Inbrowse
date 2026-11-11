#![cfg(feature = "render")]

use std::io::{Read, Write};
use std::sync::Arc;

use inbrowse::{BrowserContext, Page, RasterPdfOptions};

fn serve(body: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://{address}/")
}

fn utf16_hex(text: &str) -> String {
    text.encode_utf16().map(|unit| format!("{unit:04X}")).collect()
}

#[tokio::test(flavor = "current_thread")]
async fn printed_pages_carry_text_headers_and_an_outline() {
    std::env::set_var("INBROWSE_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve(concat!(
        "<!doctype html><html><head><title>Report</title></head>",
        "<body style=\"margin:0;font:16px sans-serif\">",
        "<h1>Summary</h1><p>Quarterly r\u{e9}sum\u{e9} for the team.</p>",
        "<h2>Numbers</h2><p style=\"height:2000px\">Growth</p>",
        "</body></html>"
    ));
    let ctx = Arc::new(BrowserContext::with_storage_and_network("pdf".into(), None, false, None, None, true));
    let mut page = Page::new("pdf-page".into(), ctx);
    page.set_viewport((1280.0, 720.0));
    page.navigate(&url).await.unwrap();

    let pdf = page
        .raster_pdf(RasterPdfOptions {
            display_header_footer: true,
            footer_template: "<div style=\"font-size:10px\"><span class=\"pageNumber\"></span>/<span class=\"totalPages\"></span></div>".into(),
            generate_document_outline: true,
            ..RasterPdfOptions::default()
        })
        .expect("PDF export");
    let raw = String::from_utf8_lossy(&pdf);

    // Letter with 1cm margins: the print layout is ~740px wide, so the 2000px
    // paragraph runs onto a third page.
    let pages = raw.matches("/Type /Page ").count();
    assert!(pages >= 3, "{pages} pages");
    assert!(raw.contains(&utf16_hex("Quarterly")), "body text is in the text layer");
    assert!(raw.contains(&utf16_hex("r\u{e9}sum\u{e9}")), "accented text keeps its code points");
    assert!(raw.contains(&format!("(1/{pages}) Tj")), "footer carries page values");
    assert!(raw.contains("/Outlines") && raw.contains("/PageMode /UseOutlines"));
}

/// The first embedded page raster, decoded from its JPEG stream.
fn first_page_raster(pdf: &[u8]) -> image::RgbImage {
    let find = |from: usize, needle: &[u8]| {
        from + pdf[from..]
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap_or_else(|| panic!("{} not found", String::from_utf8_lossy(needle)))
    };
    let image_object = find(0, b"/Subtype /Image");
    let length_start = find(image_object, b"/Length ") + b"/Length ".len();
    let length_end = length_start + pdf[length_start..].iter().take_while(|byte| byte.is_ascii_digit()).count();
    let length: usize = std::str::from_utf8(&pdf[length_start..length_end]).unwrap().parse().unwrap();
    let stream_start = find(length_end, b"stream\n") + b"stream\n".len();
    image::load_from_memory_with_format(&pdf[stream_start..stream_start + length], image::ImageFormat::Jpeg)
        .expect("page JPEG")
        .into_rgb8()
}

#[tokio::test(flavor = "current_thread")]
async fn printed_pages_paint_iframe_documents() {
    std::env::set_var("INBROWSE_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve(concat!(
        "<!doctype html><html style=\"margin:0\"><body style=\"margin:0\">",
        "<iframe style=\"position:absolute;left:20px;top:20px;width:100px;height:60px;border:0\" ",
        "srcdoc=\"<body style='margin:0;background:rgb(224,32,32)'></body>\"></iframe>",
        "</body></html>"
    ));
    let ctx = Arc::new(BrowserContext::with_storage_and_network("pdf-frame".into(), None, false, None, None, true));
    let mut page = Page::new("pdf-frame-page".into(), ctx);
    page.set_viewport((400.0, 300.0));
    page.navigate(&url).await.unwrap();

    let pdf = page
        .raster_pdf(RasterPdfOptions {
            print_background: true,
            paper_width_in: 400.0 / 96.0,
            paper_height_in: 300.0 / 96.0,
            margin_top_in: 0.0,
            margin_bottom_in: 0.0,
            margin_left_in: 0.0,
            margin_right_in: 0.0,
            ..RasterPdfOptions::default()
        })
        .expect("PDF export");
    let raster = first_page_raster(&pdf);
    let near = |pixel: &image::Rgb<u8>, expected: [u8; 3]| {
        pixel.0.iter().zip(expected).all(|(got, want)| got.abs_diff(want) <= 8)
    };
    assert!(near(raster.get_pixel(70, 50), [224, 32, 32]), "frame center: {:?}", raster.get_pixel(70, 50));
    assert!(near(raster.get_pixel(10, 50), [255, 255, 255]), "left of the frame");
    assert!(near(raster.get_pixel(70, 100), [255, 255, 255]), "below the frame");
}
