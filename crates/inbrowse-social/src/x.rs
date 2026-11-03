//! X (Twitter). Two routes:
//! 1. Signed in (cookies): `inbrowse-mcp` opens [`search_page_url`] in a
//!    browser page with the user's cookies and hands every GraphQL response the
//!    page receives to [`items_from_graphql`]. X's own JavaScript signs the
//!    requests (its `x-client-transaction-id` is computed by obfuscated code
//!    and its operation ids change), so nothing here has to track either.
//! 2. `credentials` with provider "x": the official API v2 recent search
//!    ([`search_api`]), a bearer token on a paid plan; covers the last 7 days.
//!
//! Called from `lib.rs::search` (API route) and `inbrowse-mcp` (page route).

use std::collections::HashSet;

use serde_json::Value;

use crate::http::{self, i, s, Req};
use crate::{time, Credentials, DateConfidence, Engagement, Item, SearchRequest, Session, SocialError, Sort};

const SITE: &str = "X";
/// Recent search on the official API reaches back this far.
pub const API_MAX_DAYS: u32 = 7;

/// The x.com search page for a query. `since:` bounds the window; `f=live`
/// lists newest first, otherwise X's "Top" ranking.
pub fn search_page_url(query: &str, days: u32, sort: Sort) -> String {
    let since = time::to_date(time::now() - i64::from(days.max(1)) * 86_400);
    let q = format!("{query} since:{since}");
    let mut url = url::Url::parse("https://x.com/search").expect("static URL");
    url.query_pairs_mut().append_pair("q", &q).append_pair("src", "typed_query");
    if sort == Sort::Recent {
        url.query_pairs_mut().append_pair("f", "live");
    }
    url.to_string()
}

fn post_url(screen_name: Option<&str>, id: &str) -> String {
    match screen_name {
        Some(name) => format!("https://x.com/{name}/status/{id}"),
        None => format!("https://x.com/i/status/{id}"),
    }
}

/// One `Tweet` result object (already unwrapped) to an item.
fn tweet_item(t: &Value) -> Option<Item> {
    let id = s(t, "rest_id")?;
    let legacy = t.get("legacy")?;
    let user = t.pointer("/core/user_results/result");
    let screen = user.and_then(|u| {
        u.pointer("/legacy/screen_name")
            .or_else(|| u.pointer("/core/screen_name"))
            .and_then(Value::as_str)
            .map(String::from)
    });
    let text = t
        .pointer("/note_tweet/note_tweet_results/result/text")
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| s(legacy, "full_text"))
        .unwrap_or_default();
    let secs = s(legacy, "created_at").and_then(|d| time::parse_twitter(&d));
    Some(Item {
        source: "x",
        url: post_url(screen.as_deref(), &id),
        id,
        title: None,
        text,
        author: screen,
        community: None,
        published_at: secs.map(time::to_rfc3339),
        date_confidence: if secs.is_some() { DateConfidence::Exact } else { DateConfidence::Unknown },
        engagement: Engagement {
            likes: i(legacy, "favorite_count"),
            reposts: i(legacy, "retweet_count"),
            comments: i(legacy, "reply_count"),
            quotes: i(legacy, "quote_count"),
            views: t.pointer("/views/count").and_then(|v| match v {
                Value::String(s) => s.parse().ok(),
                Value::Number(n) => n.as_i64(),
                _ => None,
            }),
            ..Default::default()
        },
        rank_score: 0.0,
        top_comments: Vec::new(),
        published_secs: secs,
    })
}

/// Every post in a GraphQL response, whatever the operation is called today:
/// walk the JSON for `tweet_results.result`, unwrap visibility wrappers, skip
/// tombstones, and do not descend into a post (its quoted post is not a result).
pub fn items_from_graphql(body: &Value) -> Vec<Item> {
    fn walk(v: &Value, out: &mut Vec<Item>, seen: &mut HashSet<String>) {
        match v {
            Value::Object(map) => {
                if let Some(result) = map.get("tweet_results").and_then(|r| r.get("result")) {
                    let tweet = if result.get("__typename").and_then(Value::as_str) == Some("TweetWithVisibilityResults") {
                        result.get("tweet").unwrap_or(result)
                    } else {
                        result
                    };
                    if let Some(item) = tweet_item(tweet) {
                        if seen.insert(item.id.clone()) {
                            out.push(item);
                        }
                    }
                    return;
                }
                for child in map.values() {
                    walk(child, out, seen);
                }
            }
            Value::Array(list) => {
                for child in list {
                    walk(child, out, seen);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(body, &mut out, &mut HashSet::new());
    out
}

/// Official API v2 recent search.
pub(crate) async fn search_api(
    session: &Session,
    req: &SearchRequest,
    credentials: &Credentials,
    warnings: &mut Vec<String>,
) -> Result<Vec<Item>, SocialError> {
    let days = if req.days > API_MAX_DAYS {
        warnings.push(format!("the X API's recent search covers {API_MAX_DAYS} days; window shortened"));
        API_MAX_DAYS
    } else {
        req.days
    };
    // The API refuses a start time older than 7 days, measured on its clock.
    let start = time::now() - i64::from(days) * 86_400 + 60;
    let mut url = http::url("https://api.x.com/2/tweets/search/recent")?;
    url.query_pairs_mut()
        .append_pair("query", &req.query)
        .append_pair("max_results", &req.limit.clamp(10, 100).to_string())
        .append_pair("start_time", &time::to_rfc3339(start))
        .append_pair("tweet.fields", "created_at,public_metrics,author_id,lang")
        .append_pair("expansions", "author_id")
        .append_pair("user.fields", "username")
        .append_pair("sort_order", if req.sort == Sort::Recent { "recency" } else { "relevancy" });
    let options = Req {
        headers: vec![("Authorization", format!("Bearer {}", credentials.key)), ("Accept", "application/json".into())],
        cookies: false,
        ..Req::default()
    };
    let body = match http::json(session, &url, options, SITE).await {
        // The API answers 403 for a plan without search access.
        Err(SocialError::Blocked(_)) => {
            return Err(SocialError::QuotaExceeded(
                "the X API key's plan does not include recent search".into(),
            ))
        }
        other => other?,
    };
    Ok(items_from_api(&body))
}

fn items_from_api(body: &Value) -> Vec<Item> {
    let users: Vec<(String, String)> = body
        .pointer("/includes/users")
        .and_then(Value::as_array)
        .map(|u| u.iter().filter_map(|u| Some((s(u, "id")?, s(u, "username")?))).collect())
        .unwrap_or_default();
    body.get("data")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|t| {
                    let id = s(t, "id")?;
                    let author_id = s(t, "author_id");
                    let screen = author_id.and_then(|a| users.iter().find(|(id, _)| *id == a).map(|(_, n)| n.clone()));
                    let secs = s(t, "created_at").and_then(|d| time::parse_rfc3339(&d));
                    let m = t.get("public_metrics");
                    Some(Item {
                        source: "x",
                        url: post_url(screen.as_deref(), &id),
                        id,
                        title: None,
                        text: s(t, "text").unwrap_or_default(),
                        author: screen,
                        community: None,
                        published_at: secs.map(time::to_rfc3339),
                        date_confidence: if secs.is_some() { DateConfidence::Exact } else { DateConfidence::Unknown },
                        engagement: Engagement {
                            likes: m.and_then(|m| i(m, "like_count")),
                            reposts: m.and_then(|m| i(m, "retweet_count")),
                            comments: m.and_then(|m| i(m, "reply_count")),
                            quotes: m.and_then(|m| i(m, "quote_count")),
                            views: m.and_then(|m| i(m, "impression_count")),
                            ..Default::default()
                        },
                        rank_score: 0.0,
                        top_comments: Vec::new(),
                        published_secs: secs,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRAPHQL: &str = r#"{"data":{"search_by_raw_query":{"search_timeline":{"timeline":{"instructions":[
      {"type":"TimelineAddEntries","entries":[
        {"content":{"itemContent":{"tweet_results":{"result":{"__typename":"Tweet","rest_id":"111",
          "core":{"user_results":{"result":{"core":{"screen_name":"alice"}}}},
          "views":{"count":"5400"},
          "legacy":{"full_text":"short text","created_at":"Tue Oct 13 10:27:00 +0000 2026",
            "favorite_count":42,"retweet_count":7,"reply_count":3,"quote_count":1},
          "quoted_status_result":{"result":{"rest_id":"999","legacy":{"full_text":"quoted"}}}}}}}},
        {"content":{"itemContent":{"tweet_results":{"result":{"__typename":"TweetWithVisibilityResults",
          "tweet":{"rest_id":"222","core":{"user_results":{"result":{"legacy":{"screen_name":"bob"}}}},
          "note_tweet":{"note_tweet_results":{"result":{"text":"a long post"}}},
          "legacy":{"full_text":"a long po…","created_at":"Tue Oct 13 11:00:00 +0000 2026","favorite_count":1}}}}}}},
        {"content":{"itemContent":{"tweet_results":{"result":{"__typename":"TweetTombstone"}}}}},
        {"content":{"cursorType":"Bottom","value":"abc"}}
      ]}]}}}}}"#;

    #[test]
    fn graphql_walk() {
        let v: Value = serde_json::from_str(GRAPHQL).unwrap();
        let items = items_from_graphql(&v);
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["111", "222"], "quoted post and tombstone are not results");
        assert_eq!(items[0].url, "https://x.com/alice/status/111");
        assert_eq!(items[0].engagement.views, Some(5400));
        assert_eq!(items[0].published_at.as_deref(), Some("2026-10-13T10:27:00Z"));
        assert_eq!(items[1].text, "a long post");
        assert_eq!(items[1].author.as_deref(), Some("bob"));
    }

    #[test]
    fn api_v2_shape() {
        let v: Value = serde_json::from_str(
            r#"{"data":[{"id":"5","text":"hi","author_id":"9","created_at":"2026-10-13T10:27:00.000Z",
               "public_metrics":{"like_count":4,"retweet_count":2,"reply_count":1,"quote_count":0,"impression_count":100}}],
               "includes":{"users":[{"id":"9","username":"carol"}]}}"#,
        )
        .unwrap();
        let items = items_from_api(&v);
        assert_eq!(items[0].url, "https://x.com/carol/status/5");
        assert_eq!(items[0].engagement.views, Some(100));
    }

    #[test]
    fn page_url_has_window_and_order() {
        let u = search_page_url("rust lang", 7, Sort::Recent);
        assert!(u.starts_with("https://x.com/search?q=rust+lang+since%3A"), "{u}");
        assert!(u.ends_with("&f=live"));
    }
}
