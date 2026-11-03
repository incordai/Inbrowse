//! Reddit search and threads. Three routes, chosen per call:
//! 1. `credentials` with provider "reddit": the official Data API with
//!    app-only OAuth (client id + secret of the user's own app). Free tier,
//!    never touches the user's account. Recommended.
//! 2. Reddit cookies in the session's jar: the site's `.json` endpoints as
//!    the signed-in user.
//! 3. Anonymous `.json`: usually refused with 403 now, reported as `blocked:`.
//!
//! Reddit asks for at most one request a second, so requests are paced at
//! that. Called from `lib.rs`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::Value;
use url::Url;

use crate::http::{self, f, i, s, Req};
use crate::{time, Auth, Comment, Credentials, DateConfidence, Engagement, Item, SearchRequest, Session, SocialError, Sort};

const SITE: &str = "Reddit";
const GAP: Duration = Duration::from_millis(1000);
/// Reddit's signed-in session cookie.
const LOGIN_COOKIE: &str = "reddit_session";

/// App tokens by client id, reused until shortly before they expire. Held in
/// memory for this process only.
fn tokens() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    static T: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn app_token(session: &Session, c: &Credentials) -> Result<String, SocialError> {
    if let Some((token, until)) = tokens().lock().unwrap_or_else(|e| e.into_inner()).get(&c.key) {
        if *until > Instant::now() {
            return Ok(token.clone());
        }
    }
    let secret = c.secret.as_deref().unwrap_or("");
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("{}:{secret}", c.key));
    let req = Req {
        headers: vec![("Authorization", format!("Basic {basic}")), ("Accept", "application/json".into())],
        cookies: false,
        form: Some("grant_type=client_credentials".into()),
        min_gap: GAP,
        login_cookie: None,
    };
    let body = http::json(session, &http::url("https://www.reddit.com/api/v1/access_token")?, req, SITE).await?;
    let token = s(&body, "access_token").ok_or_else(|| {
        SocialError::LoginRequired("Reddit did not accept the app id and secret".into())
    })?;
    let ttl = i(&body, "expires_in").unwrap_or(3600).max(120) as u64;
    tokens()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(c.key.clone(), (token.clone(), Instant::now() + Duration::from_secs(ttl - 60)));
    Ok(token)
}

enum Route {
    Api(String),
    Site { auth: Auth },
}

async fn route(session: &Session, credentials: Option<&Credentials>) -> Result<Route, SocialError> {
    match credentials {
        Some(c) if c.provider.eq_ignore_ascii_case("reddit") => Ok(Route::Api(app_token(session, c).await?)),
        Some(c) => Err(SocialError::Invalid(format!("credentials for {} cannot be used with Reddit", c.provider))),
        None => {
            // Reddit sets other cookies on anonymous visits; only this one
            // means a signed-in account.
            let signed_in = session.cookie(&http::url("https://www.reddit.com/")?, LOGIN_COOKIE).is_some();
            Ok(Route::Site {
                auth: if signed_in { Auth::Cookies } else { Auth::Anonymous },
            })
        }
    }
}

impl Route {
    /// `path` like `/search` or `/r/rust/search`; returns the URL and options.
    fn request(&self, path: &str) -> Result<(Url, Req<'static>, Auth), SocialError> {
        match self {
            Route::Api(token) => Ok((
                http::url(&format!("https://oauth.reddit.com{path}"))?,
                Req {
                    headers: vec![("Authorization", format!("bearer {token}")), ("Accept", "application/json".into())],
                    cookies: false,
                    form: None,
                    min_gap: GAP,
                    login_cookie: None,
                },
                Auth::Api,
            )),
            Route::Site { auth } => Ok((
                http::url(&format!("https://www.reddit.com{path}.json"))?,
                Req {
                    min_gap: GAP,
                    login_cookie: Some(LOGIN_COOKIE),
                    ..Req::default()
                },
                *auth,
            )),
        }
    }
}

fn bucket(days: u32) -> &'static str {
    match days {
        0..=1 => "day",
        2..=7 => "week",
        8..=31 => "month",
        32..=365 => "year",
        _ => "all",
    }
}

fn post(d: &Value) -> Option<Item> {
    let permalink = s(d, "permalink")?;
    let secs = f(d, "created_utc").map(|t| t as i64);
    let mut text = s(d, "selftext").unwrap_or_default();
    if text.is_empty() && d.get("is_self").and_then(Value::as_bool) != Some(true) {
        text = s(d, "url").unwrap_or_default();
    }
    Some(Item {
        source: "reddit",
        id: s(d, "id").unwrap_or_else(|| permalink.clone()),
        url: format!("https://www.reddit.com{permalink}"),
        title: s(d, "title"),
        text,
        author: s(d, "author").filter(|a| a != "[deleted]"),
        community: s(d, "subreddit"),
        published_at: secs.map(time::to_rfc3339),
        date_confidence: if secs.is_some() { DateConfidence::Exact } else { DateConfidence::Unknown },
        engagement: Engagement {
            score: i(d, "score"),
            comments: i(d, "num_comments"),
            upvote_ratio: f(d, "upvote_ratio"),
            ..Default::default()
        },
        rank_score: 0.0,
        top_comments: Vec::new(),
        published_secs: secs,
    })
}

fn listing_posts(listing: &Value) -> Vec<Item> {
    listing
        .pointer("/data/children")
        .and_then(Value::as_array)
        .map(|c| c.iter().filter_map(|child| child.get("data")).filter_map(post).collect())
        .unwrap_or_default()
}

pub(crate) async fn search(
    session: &Session,
    req: &SearchRequest,
    _from: i64,
    _to: i64,
    _warnings: &mut Vec<String>,
) -> Result<(Vec<Item>, Auth), SocialError> {
    let route = route(session, req.credentials.as_ref()).await?;
    let community = req
        .community
        .as_deref()
        .map(|c| c.trim().trim_start_matches("r/").trim_start_matches("/r/"))
        .filter(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'));
    let path = match community {
        Some(sub) => format!("/r/{sub}/search"),
        None => "/search".into(),
    };
    let (mut url, options, auth) = route.request(&path)?;
    let sort = match req.sort {
        Sort::Relevance => "relevance",
        Sort::Recent => "new",
        Sort::Top => "top",
    };
    url.query_pairs_mut()
        .append_pair("q", &req.query)
        .append_pair("sort", sort)
        .append_pair("t", bucket(req.days))
        .append_pair("limit", &req.limit.min(100).to_string())
        .append_pair("type", "link")
        .append_pair("raw_json", "1");
    if community.is_some() {
        url.query_pairs_mut().append_pair("restrict_sr", "on");
    }
    let body = http::json(session, &url, options, SITE).await?;
    Ok((listing_posts(&body), auth))
}

/// The post id from `/r/<sub>/comments/<id>/...`, `/comments/<id>` or redd.it/<id>.
fn post_id(url: &Url) -> Result<String, SocialError> {
    let parts: Vec<&str> = url.path_segments().map(|s| s.filter(|p| !p.is_empty()).collect()).unwrap_or_default();
    let host = url.host_str().unwrap_or("");
    let id = if host.ends_with("redd.it") {
        parts.first().copied()
    } else {
        parts.iter().position(|p| *p == "comments").and_then(|i| parts.get(i + 1).copied())
    };
    id.filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(String::from)
        .ok_or_else(|| SocialError::Invalid("expected a Reddit post URL (/r/<sub>/comments/<id>/...)".into()))
}

pub(crate) async fn thread(
    session: &Session,
    url: &Url,
    comments: usize,
    credentials: Option<&Credentials>,
    _warnings: &mut Vec<String>,
) -> Result<(Item, Auth), SocialError> {
    let id = post_id(url)?;
    let route = route(session, credentials).await?;
    let (mut api, options, auth) = route.request(&format!("/comments/{id}"))?;
    api.query_pairs_mut()
        .append_pair("sort", "top")
        .append_pair("limit", &comments.max(1).to_string())
        .append_pair("depth", "1")
        .append_pair("raw_json", "1");
    let body = http::json(session, &api, options, SITE).await?;
    let parts = body.as_array().ok_or_else(|| SocialError::Failed("Reddit: unexpected thread shape".into()))?;
    let mut item = parts
        .first()
        .map(listing_posts)
        .and_then(|mut p| (!p.is_empty()).then(|| p.remove(0)))
        .ok_or_else(|| SocialError::Failed("Reddit: thread without a post".into()))?;
    if let Some(children) = parts.get(1).and_then(|l| l.pointer("/data/children")).and_then(Value::as_array) {
        item.top_comments = children
            .iter()
            .filter(|c| c.get("kind").and_then(Value::as_str) == Some("t1"))
            .filter_map(|c| c.get("data"))
            .filter_map(|d| {
                let body = s(d, "body")?;
                (body != "[deleted]" && body != "[removed]").then(|| Comment {
                    author: s(d, "author"),
                    text: body,
                    score: i(d, "score"),
                    published_at: f(d, "created_utc").map(|t| time::to_rfc3339(t as i64)),
                })
            })
            .take(comments)
            .collect();
    }
    Ok((item, auth))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_ids_and_buckets() {
        let u = Url::parse("https://www.reddit.com/r/rust/comments/1abcde/some_title/").unwrap();
        assert_eq!(post_id(&u).unwrap(), "1abcde");
        assert_eq!(post_id(&Url::parse("https://redd.it/1abcde").unwrap()).unwrap(), "1abcde");
        assert!(post_id(&Url::parse("https://www.reddit.com/r/rust/").unwrap()).is_err());
        assert_eq!(bucket(30), "month");
        assert_eq!(bucket(7), "week");
    }

    #[test]
    fn listing_to_items() {
        let v: Value = serde_json::from_str(
            r#"{"data":{"children":[{"kind":"t3","data":{"id":"1abcde","title":"Hello","selftext":"",
              "is_self":false,"url":"https://example.com/a","author":"bob","subreddit":"rust",
              "created_utc":1791887220.0,"score":120,"num_comments":45,"upvote_ratio":0.97,
              "permalink":"/r/rust/comments/1abcde/hello/"}}]}}"#,
        )
        .unwrap();
        let items = listing_posts(&v);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].url, "https://www.reddit.com/r/rust/comments/1abcde/hello/");
        assert_eq!(items[0].text, "https://example.com/a");
        assert_eq!(items[0].engagement.upvote_ratio, Some(0.97));
        assert_eq!(items[0].published_at.as_deref(), Some("2026-10-13T10:27:00Z"));
    }
}
