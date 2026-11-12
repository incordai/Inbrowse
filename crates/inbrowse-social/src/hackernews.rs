//! Hacker News through the public Algolia API (search) and HN's own Firebase
//! API (comment display order). No key needed. Called from `lib.rs`.

use serde_json::Value;
use url::Url;

use crate::http::{self, i, s, Req};
use crate::{
    time, Comment, DateConfidence, Engagement, Item, SearchRequest, Session, SocialError, Sort,
};

const SITE: &str = "Hacker News";

fn item_url(id: &str) -> String {
    format!("https://news.ycombinator.com/item?id={id}")
}

/// HN comment and story text is HTML: drop tags, decode the entities HN uses,
/// and turn paragraphs into newlines.
pub(crate) fn strip_html(html: &str) -> String {
    let html = html.replace("<p>", "\n\n").replace("<br>", "\n");
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&#x2F;", "/")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

fn story(hit: &Value) -> Option<Item> {
    let id = s(hit, "objectID")?;
    let secs = i(hit, "created_at_i");
    let link = s(hit, "url");
    let mut text = hit
        .get("story_text")
        .and_then(Value::as_str)
        .map(strip_html)
        .unwrap_or_default();
    if let Some(link) = &link {
        if text.is_empty() {
            text = link.clone();
        }
    }
    Some(Item {
        source: "hackernews",
        url: item_url(&id),
        id,
        title: s(hit, "title"),
        text,
        author: s(hit, "author"),
        community: None,
        published_at: secs.map(time::to_rfc3339),
        date_confidence: if secs.is_some() {
            DateConfidence::Exact
        } else {
            DateConfidence::Unknown
        },
        engagement: Engagement {
            score: i(hit, "points"),
            comments: i(hit, "num_comments"),
            ..Default::default()
        },
        rank_score: 0.0,
        top_comments: Vec::new(),
        published_secs: secs,
    })
}

pub(crate) async fn search(
    session: &Session,
    req: &SearchRequest,
    from: i64,
    to: i64,
) -> Result<Vec<Item>, SocialError> {
    let endpoint = if req.sort == Sort::Recent {
        "search_by_date"
    } else {
        "search"
    };
    let mut url = http::url(&format!("https://hn.algolia.com/api/v1/{endpoint}"))?;
    // Algolia ANDs every word; optionalWords lets longer queries still match.
    let words: Vec<&str> = req.query.split_whitespace().collect();
    url.query_pairs_mut()
        .append_pair("query", &req.query)
        .append_pair("tags", "story")
        .append_pair(
            "numericFilters",
            &format!("created_at_i>{from},created_at_i<{to}"),
        )
        .append_pair("hitsPerPage", &(req.limit * 2).min(200).to_string());
    if words.len() > 2 {
        url.query_pairs_mut()
            .append_pair("optionalWords", &words.join(" "));
    }
    let body = http::json(session, &url, Req::default(), SITE).await?;
    Ok(body
        .get("hits")
        .and_then(Value::as_array)
        .map(|hits| hits.iter().filter_map(story).collect())
        .unwrap_or_default())
}

fn item_id(url: &Url) -> Result<String, SocialError> {
    url.query_pairs()
        .find(|(k, _)| k == "id")
        .map(|(_, v)| v.into_owned())
        .filter(|v| v.chars().all(|c| c.is_ascii_digit()) && !v.is_empty())
        .ok_or_else(|| {
            SocialError::Invalid("expected https://news.ycombinator.com/item?id=<number>".into())
        })
}

pub(crate) async fn thread(
    session: &Session,
    url: &Url,
    comments: usize,
) -> Result<Item, SocialError> {
    let id = item_id(url)?;
    let tree = http::json(
        session,
        &http::url(&format!("https://hn.algolia.com/api/v1/items/{id}"))?,
        Req::default(),
        SITE,
    )
    .await?;
    // Algolia's children are not in HN's display order; Firebase's kids are.
    let order: Vec<i64> = if comments > 0 {
        let fb = http::json(
            session,
            &http::url(&format!(
                "https://hacker-news.firebaseio.com/v0/item/{id}.json"
            ))?,
            Req::default(),
            SITE,
        )
        .await?;
        fb.get("kids")
            .and_then(Value::as_array)
            .map(|k| k.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let children = tree
        .get("children")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut top = Vec::new();
    for kid in order {
        if top.len() >= comments {
            break;
        }
        let Some(c) = children.iter().find(|c| i(c, "id") == Some(kid)) else {
            continue;
        };
        let text = c
            .get("text")
            .and_then(Value::as_str)
            .map(strip_html)
            .unwrap_or_default();
        if text.is_empty() {
            continue; // deleted or dead
        }
        top.push(Comment {
            author: s(c, "author"),
            text,
            score: None,
            published_at: i(c, "created_at_i").map(time::to_rfc3339),
        });
    }
    let secs = i(&tree, "created_at_i");
    let link = s(&tree, "url");
    let mut text = tree
        .get("text")
        .and_then(Value::as_str)
        .map(strip_html)
        .unwrap_or_default();
    if text.is_empty() {
        text = link.unwrap_or_default();
    }
    Ok(Item {
        source: "hackernews",
        url: item_url(&id),
        id,
        title: s(&tree, "title"),
        text,
        author: s(&tree, "author"),
        community: None,
        published_at: secs.map(time::to_rfc3339),
        date_confidence: if secs.is_some() {
            DateConfidence::Exact
        } else {
            DateConfidence::Unknown
        },
        engagement: Engagement {
            score: i(&tree, "points"),
            comments: Some(children.len() as i64),
            ..Default::default()
        },
        rank_score: 0.0,
        top_comments: top,
        published_secs: secs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_to_item() {
        let hit: Value = serde_json::from_str(
            r#"{"objectID":"50069499","title":"11 year old unsoundness bugs","author":"formerly_proven",
                "points":751,"num_comments":446,"created_at_i":1791887220,"url":"https://github.com/x"}"#,
        )
        .unwrap();
        let item = story(&hit).unwrap();
        assert_eq!(item.url, "https://news.ycombinator.com/item?id=50069499");
        assert_eq!(item.published_at.as_deref(), Some("2026-10-13T10:27:00Z"));
        assert_eq!(item.engagement.score, Some(751));
        assert_eq!(item.text, "https://github.com/x");
        assert_eq!(strip_html("a<p>b &amp; <i>c</i>&#x27;"), "a\n\nb & c'");
    }
}
