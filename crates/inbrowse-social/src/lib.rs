//! Research sources for agents: posts with engagement counts from social
//! sites and forums, in one item shape, ranked and de-duplicated.
//!
//! Entry points: [`search`] and [`thread`]. Callers: `inbrowse-mcp`
//! (`social_search`, `social_thread` tools). Each source lives in its own
//! module (`hackernews.rs`, `github.rs`, `polymarket.rs`, `reddit.rs`); they
//! share `http.rs` (requests, pacing, error codes), `rank.rs` (scores,
//! de-duplication) and `time.rs` (dates).
//!
//! Authentication per call, in this order: `credentials` (an API key the
//! caller passes for this call only), else cookies already in the session's
//! cookie jar (set through the MCP cookie tools), else anonymous. Nothing here
//! stores a credential, and no error or output repeats one.

mod github;
mod hackernews;
mod http;
mod polymarket;
mod rank;
mod reddit;
mod time;
mod x;

use std::sync::Arc;

use inbrowse_net::CookieJar;
use serde::{Deserialize, Serialize};

pub use http::Session;
pub use x::{items_from_graphql as x_items_from_graphql, search_page_url as x_search_page_url};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Reddit,
    #[serde(rename = "hackernews")]
    HackerNews,
    Github,
    Polymarket,
    X,
}

impl Source {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "reddit" => Some(Self::Reddit),
            "hackernews" | "hn" | "hacker_news" => Some(Self::HackerNews),
            "github" => Some(Self::Github),
            "polymarket" => Some(Self::Polymarket),
            "x" | "twitter" => Some(Self::X),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reddit => "reddit",
            Self::HackerNews => "hackernews",
            Self::Github => "github",
            Self::Polymarket => "polymarket",
            Self::X => "x",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Relevance,
    Recent,
    Top,
}

impl Sort {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "relevance" => Some(Self::Relevance),
            "recent" => Some(Self::Recent),
            "top" => Some(Self::Top),
            _ => None,
        }
    }
}

/// An API key for one call. `Debug` hides the secret parts.
#[derive(Clone, Deserialize)]
pub struct Credentials {
    pub kind: String,
    pub provider: String,
    pub key: String,
    #[serde(default)]
    pub secret: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("kind", &self.kind)
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub source: Source,
    pub query: String,
    pub days: u32,
    pub limit: usize,
    pub sort: Sort,
    /// Subreddit for Reddit; ignored elsewhere.
    pub community: Option<String>,
    pub credentials: Option<Credentials>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Engagement {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub likes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reposts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quotes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub views: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upvote_ratio: Option<f64>,
    /// Prediction markets: traded volume and liquidity in USD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liquidity: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DateConfidence {
    Exact,
    Approximate,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct Comment {
    pub author: Option<String>,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<i64>,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub source: &'static str,
    pub id: String,
    pub url: String,
    pub title: Option<String>,
    pub text: String,
    pub author: Option<String>,
    pub community: Option<String>,
    pub published_at: Option<String>,
    pub date_confidence: DateConfidence,
    pub engagement: Engagement,
    pub rank_score: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top_comments: Vec<Comment>,
    /// Unix seconds, for window filtering and sorting; not serialised.
    #[serde(skip)]
    pub(crate) published_secs: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Auth {
    Api,
    Cookies,
    Anonymous,
}

#[derive(Debug, Clone, Serialize)]
pub struct Window {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Results {
    pub source: &'static str,
    pub query: Option<String>,
    pub window: Window,
    pub auth: Auth,
    pub untrusted: bool,
    pub items: Vec<Item>,
    pub warnings: Vec<String>,
}

/// Errors carry the prefix codes Watchman maps to user notices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocialError {
    /// Cookies missing or expired, or the API key was rejected.
    LoginRequired(String),
    /// The site wants a checkpoint or captcha solved in a normal browser.
    AccountChallenge(String),
    RateLimited { retry_after_secs: u64, detail: String },
    QuotaExceeded(String),
    /// Anonymous access refused; an account or API key would help.
    Blocked(String),
    /// Bad arguments or an unsupported URL.
    Invalid(String),
    /// Network failure or a response this crate could not read.
    Failed(String),
}

impl std::fmt::Display for SocialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoginRequired(d) => write!(f, "login_required: {d}"),
            Self::AccountChallenge(d) => write!(f, "account_challenge: {d}"),
            Self::RateLimited { retry_after_secs, detail } => write!(f, "rate_limited:{retry_after_secs}: {detail}"),
            Self::QuotaExceeded(d) => write!(f, "quota_exceeded: {d}"),
            Self::Blocked(d) => write!(f, "blocked: {d}"),
            Self::Invalid(d) => write!(f, "invalid: {d}"),
            Self::Failed(d) => write!(f, "failed: {d}"),
        }
    }
}

impl std::error::Error for SocialError {}

pub const MAX_LIMIT: usize = 100;
pub const MAX_DAYS: u32 = 365;

/// Build a session over an existing cookie jar (the browser context's).
pub fn session(cookie_jar: Arc<CookieJar>, proxy: Option<String>, user_agent: String) -> Session {
    Session::new(cookie_jar, proxy, user_agent)
}

/// Search one source for posts in the last `days` days.
pub async fn search(session: &Session, request: &SearchRequest) -> Result<Results, SocialError> {
    if request.query.trim().is_empty() {
        return Err(SocialError::Invalid("query is empty".into()));
    }
    let request = SearchRequest {
        days: request.days.clamp(1, MAX_DAYS),
        limit: request.limit.clamp(1, MAX_LIMIT),
        ..request.clone()
    };
    let (from, to) = window(request.days);
    let mut warnings = Vec::new();
    let (items, auth) = match request.source {
        Source::HackerNews => (hackernews::search(session, &request, from, to).await?, Auth::Anonymous),
        Source::Github => github::search(session, &request, from, to, &mut warnings).await?,
        Source::Polymarket => (polymarket::search(session, &request, &mut warnings).await?, Auth::Anonymous),
        Source::Reddit => reddit::search(session, &request, from, to, &mut warnings).await?,
        Source::X => match request.credentials.as_ref() {
            Some(c) if c.provider.eq_ignore_ascii_case("x") => {
                (x::search_api(session, &request, c, &mut warnings).await?, Auth::Api)
            }
            Some(c) => return Err(SocialError::Invalid(format!("credentials for {} cannot be used with X", c.provider))),
            None => {
                return Err(SocialError::Invalid(
                    "X without an API key is searched through a signed-in browser page".into(),
                ))
            }
        },
    };
    Ok(finish(&request, from, to, items, auth, warnings))
}

/// Window filter, ranking, de-duplication and the result envelope. Shared by
/// [`search`] and by callers that collect items themselves (the MCP page route
/// for X). `request.days` and `request.limit` must already be clamped.
pub fn finish(
    request: &SearchRequest,
    from: i64,
    to: i64,
    mut items: Vec<Item>,
    auth: Auth,
    mut warnings: Vec<String>,
) -> Results {
    // Items whose date is known and outside the window are dropped; markets
    // carry their last update, which is always recent, so this keeps them.
    let before = items.len();
    items.retain(|i| i.published_secs.is_none_or(|t| t >= from && t <= to + 3600));
    if items.len() < before {
        warnings.push(format!(
            "{} items outside the {}-day window were dropped",
            before - items.len(),
            request.days
        ));
    }
    rank::rank(&mut items, request.source, request.sort);
    rank::dedupe(&mut items);
    items.truncate(request.limit);
    Results {
        source: request.source.as_str(),
        query: Some(request.query.clone()),
        window: Window {
            from: time::to_rfc3339(from),
            to: time::to_rfc3339(to),
        },
        auth,
        untrusted: true,
        items,
        warnings,
    }
}

/// The search window of `days` days ending now, as Unix seconds.
pub fn window(days: u32) -> (i64, i64) {
    let to = time::now();
    (to - i64::from(days.clamp(1, MAX_DAYS)) * 86_400, to)
}

/// One post and its top `comments` comments, from its URL.
pub async fn thread(
    session: &Session,
    url: &str,
    comments: usize,
    credentials: Option<&Credentials>,
) -> Result<Results, SocialError> {
    let parsed = url::Url::parse(url.trim()).map_err(|_| SocialError::Invalid(format!("not a URL: {url}")))?;
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    let host = host.trim_start_matches("www.").trim_start_matches("old.").trim_start_matches("m.");
    let comments = comments.clamp(0, 50);
    let mut warnings = Vec::new();
    let (source, item, auth) = match host {
        "news.ycombinator.com" => (Source::HackerNews, hackernews::thread(session, &parsed, comments).await?, Auth::Anonymous),
        "github.com" => {
            let (item, auth) = github::thread(session, &parsed, comments, credentials, &mut warnings).await?;
            (Source::Github, item, auth)
        }
        "reddit.com" | "redd.it" => {
            let (item, auth) = reddit::thread(session, &parsed, comments, credentials, &mut warnings).await?;
            (Source::Reddit, item, auth)
        }
        _ => return Err(SocialError::Invalid(format!("no thread reader for {host}"))),
    };
    let now = time::now();
    Ok(Results {
        source: source.as_str(),
        query: None,
        window: Window {
            from: item.published_at.clone().unwrap_or_else(|| time::to_rfc3339(now)),
            to: time::to_rfc3339(now),
        },
        auth,
        untrusted: true,
        items: vec![item],
        warnings,
    })
}
