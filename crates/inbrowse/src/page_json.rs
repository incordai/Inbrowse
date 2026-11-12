//! The page as one JSON object, for `fetch --dump json` and the MCP
//! `browser_markdown` tool with `format: "json"`.

use serde::ser::{Serialize, SerializeMap, Serializer};
use serde::Deserialize;

use crate::Page;

/// An insertion-ordered JSON object whose values are all strings.
#[derive(Debug)]
pub struct StringMap(Vec<(String, String)>);

impl Serialize for StringMap {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for StringMap {
    /// From the `[key, value]` pairs the page script returns.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<(String, String)>::deserialize(deserializer).map(StringMap)
    }
}

/// What the page script reports. Each field is a field of [`PageJson`].
#[derive(Deserialize)]
struct Collected {
    title: String,
    description: String,
    lang: String,
    #[serde(rename = "publishedTime")]
    published_time: String,
    content: String,
    links: StringMap,
    images: StringMap,
}

/// One page: where it ended up and what it says. Serializes with the field
/// names below; `status`, `lang` and `publishedTime` are left out when unknown.
#[derive(Debug, serde::Serialize)]
pub struct PageJson {
    /// The page's URL after redirects.
    pub url: String,
    /// HTTP status of the main document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// The document title.
    pub title: String,
    /// The meta, Open Graph or Twitter description; empty when there is none.
    pub description: String,
    /// The `lang` of the `<html>` element.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// When the page says it was published, as the page writes it.
    #[serde(rename = "publishedTime", skip_serializing_if = "Option::is_none")]
    pub published_time: Option<String>,
    /// The markdown `--dump markdown` gives.
    pub content: String,
    /// Absolute link URL to link text, in document order.
    pub links: StringMap,
    /// Absolute image URL to alt text, in document order.
    pub images: StringMap,
}

/// The status of the response the current document came from: the latest
/// navigation to its URL, or else the first one.
fn main_document_status(page: &Page) -> Option<u16> {
    let current = page.url.as_ref().map(|url| {
        let mut url = url.clone();
        url.set_fragment(None);
        url
    });
    let documents = || {
        page.network_events
            .iter()
            .filter(|event| event.resource_type == "Document")
    };
    documents()
        .rev()
        .find(|event| {
            url::Url::parse(&event.url).ok().is_some_and(|mut url| {
                url.set_fragment(None);
                Some(url) == current
            })
        })
        .or_else(|| documents().next())
        .map(|event| event.status)
}

/// Collects the page as a [`PageJson`]; with `selector`, `content`, `links`
/// and `images` come from that element's first match. `Ok(None)` when the
/// selector matches nothing. With `drop_boilerplate` the page's navigation,
/// header, footer and sidebars are left out of those three. `evaluate` runs the
/// collecting script on the page, so a caller can bound it as it bounds its
/// other scripts; `Page::evaluate` runs it as is.
pub fn page_json(
    page: &mut Page,
    selector: Option<&str>,
    drop_boilerplate: bool,
    evaluate: fn(&mut Page, &str) -> Result<serde_json::Value, String>,
) -> Result<Option<PageJson>, String> {
    let script = inbrowse_js::markdown::page_json_js(selector, drop_boilerplate);
    let value = evaluate(page, &script)?;
    if value.is_null() {
        return Ok(None);
    }
    let collected: Collected = serde_json::from_value(value)
        .map_err(|error| format!("page JSON is malformed: {error}"))?;
    let non_empty = |text: String| Some(text).filter(|text| !text.is_empty());
    Ok(Some(PageJson {
        url: page.url_string(),
        status: main_document_status(page),
        title: collected.title,
        description: collected.description,
        lang: non_empty(collected.lang),
        published_time: non_empty(collected.published_time),
        content: collected.content,
        links: collected.links,
        images: collected.images,
    }))
}
