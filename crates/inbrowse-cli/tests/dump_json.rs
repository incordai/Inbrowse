//! `fetch --dump json`: the page as one JSON object, modelled on the `data`
//! object of Jina Reader's JSON response (https://github.com/jina-ai/reader,
//! Apache-2.0; see NOTICE).

mod common;

use common::{fetch, fetch_output, Response, Server};
use serde_json::{json, Value};

const POST: &str = r##"<!doctype html>
<html lang="en-GB"><head>
<title>  Post Title </title>
<meta name="description" content="A short description">
<meta property="og:description" content="Open Graph description">
<meta property="article:published_time" content="2024-01-15T10:00:00Z">
</head><body>
<nav><a href="/home">Home</a></nav>
<article>
  <h1>Post</h1>
  <p>Read <a href="more">more</a> and <a href="https://other.test/x">other</a>, <a href="more">again</a>.</p>
  <img src="img/a.png" alt="First"><img src="/b.png"><img src="data:image/png;base64,AAAA" alt="inline">
  <a href="/icon"><img src="/ic.png" alt="Icon link"></a>
  <a href="javascript:void(0)">nope</a> <a href="#top">top</a> <a href="/empty"></a>
</article>
<footer><a href="/privacy">Privacy</a></footer>
</body></html>"##;

fn post_server() -> Server {
    Server::start(vec![("/articles/post.html", Response::html(POST))])
}

fn dump_json(url: &str, args: &[&str]) -> Value {
    let output = fetch(url, &[&["--dump", "json"], args].concat());
    serde_json::from_str(&output).unwrap_or_else(|error| panic!("not JSON ({error}): {output}"))
}

fn map(value: &Value, key: &str) -> Vec<(String, String)> {
    value[key]
        .as_object()
        .unwrap_or_else(|| panic!("{key} is not an object: {value}"))
        .iter()
        .map(|(key, value)| (key.clone(), value.as_str().expect("string value").to_string()))
        .collect()
}

#[test]
fn the_page_fields() {
    let server = post_server();
    let url = server.url("/articles/post.html");
    let page = dump_json(&url, &[]);
    assert_eq!(page["url"], url);
    assert_eq!(page["status"], 200);
    assert_eq!(page["title"], "Post Title");
    assert_eq!(page["description"], "A short description");
    assert_eq!(page["lang"], "en-GB");
    assert_eq!(page["publishedTime"], "2024-01-15T10:00:00Z");
    assert_eq!(page["content"], fetch(&url, &["--dump", "markdown"]));
    assert!(page["content"].as_str().unwrap().starts_with("# Post"), "{}", page["content"]);
    let whole = dump_json(&url, &["--boilerplate", "keep"]);
    assert!(whole["content"].as_str().unwrap().starts_with("[Home]("), "{}", whole["content"]);
}

#[test]
fn links_and_images_are_absolute_and_keyed_by_url() {
    let server = post_server();
    let page = dump_json(&server.url("/articles/post.html"), &["--boilerplate", "keep"]);
    let mut links = map(&page, "links");
    links.sort();
    let mut expected = vec![
        (server.url("/home"), "Home"),
        (server.url("/articles/more"), "more"),
        ("https://other.test/x".to_string(), "other"),
        (server.url("/icon"), "Icon link"),
        (server.url("/articles/post.html#top"), "top"),
        (server.url("/empty"), ""),
        (server.url("/privacy"), "Privacy"),
    ]
    .into_iter()
    .map(|(url, text)| (url, text.to_string()))
    .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(links, expected, "javascript: links are left out, a repeated URL appears once");

    let mut images = map(&page, "images");
    images.sort();
    let mut expected = vec![
        (server.url("/articles/img/a.png"), "First"),
        (server.url("/b.png"), ""),
        (server.url("/ic.png"), "Icon link"),
    ]
    .into_iter()
    .map(|(url, text)| (url, text.to_string()))
    .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(images, expected, "data: images are not listed");
}

#[test]
fn links_keep_document_order() {
    let server = post_server();
    let output = fetch(&server.url("/articles/post.html"), &["--dump", "json", "--boilerplate", "keep"]);
    let positions: Vec<usize> = ["/home", "/articles/more", "https://other.test/x", "/icon", "#top", "/privacy"]
        .iter()
        .map(|link| output.find(&format!("{link}\"")).unwrap_or_else(|| panic!("{link} missing: {output}")))
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{positions:?}\n{output}");
}

#[test]
fn a_base_element_sets_the_base_of_links_and_images() {
    let server = Server::start(vec![(
        "/page",
        Response::html(r#"<base href="http://base.test/dir/"><a href="x">x</a><img src="y.png" alt="y">"#),
    )]);
    let page = dump_json(&server.url("/page"), &[]);
    assert_eq!(page["links"], json!({ "http://base.test/dir/x": "x" }));
    assert_eq!(page["images"], json!({ "http://base.test/dir/y.png": "y" }));
}

#[test]
fn a_selector_scopes_content_links_and_images() {
    let server = post_server();
    let url = server.url("/articles/post.html");
    let page = dump_json(&url, &["--selector", "article"]);
    assert_eq!(page["content"], fetch(&url, &["--dump", "markdown", "--selector", "article"]));
    let links: Vec<String> = map(&page, "links").into_iter().map(|(url, _)| url).collect();
    assert!(!links.contains(&server.url("/home")) && !links.contains(&server.url("/privacy")), "{links:?}");
    assert!(links.contains(&server.url("/articles/more")), "{links:?}");
    assert_eq!(page["title"], "Post Title", "page-level fields do not narrow");
    assert_eq!(page["lang"], "en-GB");
}

#[test]
fn a_selector_that_matches_nothing_fails() {
    let server = post_server();
    let output = fetch_output(
        &server.url("/articles/post.html"),
        &["--dump", "json", "--selector", "#missing", "--wait-until", "load"],
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("matched nothing"));
}

#[test]
fn boilerplate_is_dropped_by_default_and_kept_on_request() {
    let server = post_server();
    let url = server.url("/articles/post.html");
    for page in [dump_json(&url, &[]), dump_json(&url, &["--boilerplate", "drop"])] {
        let content = page["content"].as_str().unwrap();
        assert!(!content.contains("Home") && !content.contains("Privacy"), "{content}");
        let links: Vec<String> = map(&page, "links").into_iter().map(|(url, _)| url).collect();
        assert!(!links.contains(&server.url("/home")) && !links.contains(&server.url("/privacy")), "{links:?}");
        assert!(links.contains(&server.url("/articles/more")), "{links:?}");
    }
    let whole = dump_json(&url, &["--boilerplate", "keep"]);
    assert!(whole["content"].as_str().unwrap().contains("Privacy"));
    assert!(map(&whole, "links").iter().any(|(link, _)| *link == server.url("/privacy")));
}

#[test]
fn a_page_without_metadata() {
    let server = Server::start(vec![("/bare", Response::html("<p>Bare</p>"))]);
    let url = server.url("/bare");
    let page = dump_json(&url, &[]);
    assert_eq!(
        page,
        json!({
            "url": url,
            "status": 200,
            "title": "",
            "description": "",
            "content": "Bare",
            "links": {},
            "images": {},
        })
    );
}

#[test]
fn url_and_status_are_those_of_the_final_response() {
    let server = Server::start(vec![
        ("/old", Response::redirect("/new")),
        ("/new", Response::html("<title>New</title><p>here</p>")),
        ("/gone", Response::html("<title>Gone</title><p>missing</p>").status(404)),
    ]);
    let redirected = dump_json(&server.url("/old"), &[]);
    assert_eq!(redirected["url"], server.url("/new"));
    assert_eq!(redirected["status"], 200);
    assert_eq!(redirected["title"], "New");
    let missing = dump_json(&server.url("/gone"), &[]);
    assert_eq!(missing["status"], 404);
    assert_eq!(missing["content"], "missing");
}

#[test]
fn the_description_and_the_publication_time_have_fallbacks() {
    let server = Server::start(vec![
        (
            "/og",
            Response::html(
                r#"<meta property="og:description" content="From Open Graph"><meta itemprop="datePublished" content="2023-05-06"><p>x</p>"#,
            ),
        ),
        (
            "/ld",
            Response::html(
                r#"<script type="application/ld+json">{"@context":"https://schema.org","@graph":[{"@type":"WebSite"},{"@type":"Article","datePublished":"2022-02-03T04:05:06+00:00"}]}</script><p>x</p>"#,
            ),
        ),
        ("/time", Response::html(r#"<time itemprop="datePublished" datetime="2021-12-31">New Year's Eve</time>"#)),
    ]);
    let og = dump_json(&server.url("/og"), &[]);
    assert_eq!(og["description"], "From Open Graph");
    assert_eq!(og["publishedTime"], "2023-05-06");
    assert_eq!(dump_json(&server.url("/ld"), &[])["publishedTime"], "2022-02-03T04:05:06+00:00");
    assert_eq!(dump_json(&server.url("/time"), &[])["publishedTime"], "2021-12-31");
}

#[test]
fn the_sample_article_matches_its_markdown() {
    let sample = include_str!("fixtures/reader/sample.html");
    let server = Server::start(vec![("/docs/sample.html", Response::html(sample))]);
    let url = server.url("/docs/sample.html");
    let page = dump_json(&url, &[]);
    assert_eq!(page["title"], "Test Article: Web Crawling Guide");
    assert_eq!(page["description"], "A comprehensive guide to web crawling techniques");
    assert_eq!(page["publishedTime"], "2024-01-15T10:00:00Z");
    assert_eq!(page["lang"], "en");
    assert_eq!(page["content"], fetch(&url, &["--dump", "markdown"]));
    assert_eq!(
        map(&page, "images").iter().find(|(url, _)| url.ends_with("spider.png")),
        Some(&("https://example.com/spider.png".to_string(), "A spider crawling the web".to_string()))
    );
    assert!(map(&page, "links").contains(&(server.url("/guide/advanced"), "advanced guide".to_string())));
}
