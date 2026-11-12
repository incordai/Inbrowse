//! Requests for every source module.
//!
//! Each call gets its own `InbrowseHttpClient` on the session's cookie jar and
//! proxy, so per-call headers (API keys, CSRF tokens) never leak into the
//! browser's shared client and two calls cannot see each other's headers.
//! Same transport, SSRF gate and browser identity as page loads.
//!
//! Pacing: one request per host at a time, at least `min_gap` apart, across all
//! calls in the process. Status codes become the `SocialError` prefixes the MCP
//! layer reports (see `lib.rs`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use inbrowse_net::{CookieJar, InbrowseHttpClient, Response, SameSiteContext};
use serde_json::Value;
use url::Url;

use crate::SocialError;

pub struct Session {
    cookie_jar: Arc<CookieJar>,
    proxy: Option<String>,
    user_agent: String,
}

impl Session {
    pub(crate) fn new(
        cookie_jar: Arc<CookieJar>,
        proxy: Option<String>,
        user_agent: String,
    ) -> Self {
        Self {
            cookie_jar,
            proxy,
            user_agent,
        }
    }

    /// Value of one cookie that would be sent to `url`.
    pub(crate) fn cookie(&self, url: &Url, name: &str) -> Option<String> {
        let header = self
            .cookie_jar
            .get_cookie_header_in_context(url, SameSiteContext::SameSite);
        header.split(';').find_map(|pair| {
            let (k, v) = pair.trim().split_once('=')?;
            (k == name).then(|| v.trim_matches('"').to_string())
        })
    }

    async fn client(&self, headers: &[(&str, String)], with_cookies: bool) -> InbrowseHttpClient {
        // An API call must not carry the user's site cookies along with the key.
        let jar = if with_cookies {
            self.cookie_jar.clone()
        } else {
            Arc::new(CookieJar::new())
        };
        let client = InbrowseHttpClient::with_full_options(jar, self.proxy.as_deref(), false);
        client.set_user_agent(&self.user_agent).await;
        let map: HashMap<String, String> = headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        client.set_extra_headers(map).await;
        client
    }
}

/// Request options.
pub(crate) struct Req<'a> {
    pub headers: Vec<(&'a str, String)>,
    /// Send the session's cookies (false for API-key calls).
    pub cookies: bool,
    /// `application/x-www-form-urlencoded` body; makes it a POST.
    pub form: Option<String>,
    pub min_gap: Duration,
    /// The site's login cookie. A 403 counts as an account problem only when
    /// this cookie was in the jar before the request; any other cookie may be
    /// one the site set on an anonymous visit (Reddit's block page sets some).
    pub login_cookie: Option<&'static str>,
}

impl Default for Req<'_> {
    fn default() -> Self {
        Self {
            headers: vec![("Accept", "application/json".into())],
            cookies: true,
            form: None,
            min_gap: Duration::from_millis(250),
            login_cookie: None,
        }
    }
}

fn next_slot() -> &'static Mutex<HashMap<String, Instant>> {
    static SLOTS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Reserve this host's next request slot and wait for it.
async fn pace(url: &Url, gap: Duration) {
    let host = url.host_str().unwrap_or("").to_string();
    let wait = {
        let mut slots = next_slot().lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let at = slots
            .get(&host)
            .copied()
            .filter(|t| *t > now)
            .unwrap_or(now);
        slots.insert(host, at + gap);
        at - now
    };
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
}

fn header<'a>(resp: &'a Response, name: &str) -> Option<&'a str> {
    resp.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Seconds until a rate limit lifts, from the headers sites use.
fn retry_after(resp: &Response) -> u64 {
    let now = crate::time::now();
    if let Some(v) = header(resp, "retry-after").and_then(|v| v.trim().parse::<u64>().ok()) {
        return v;
    }
    if let Some(v) = header(resp, "x-ratelimit-reset").and_then(|v| v.trim().parse::<f64>().ok()) {
        // Reddit sends seconds remaining; X and GitHub send an epoch.
        let v = v as i64;
        return if v > 1_000_000_000 {
            (v - now).max(1) as u64
        } else {
            v.max(1) as u64
        };
    }
    if let Some(v) = header(resp, "x-rate-limit-reset").and_then(|v| v.trim().parse::<i64>().ok()) {
        return (v - now).max(1) as u64;
    }
    60
}

fn looks_like_html(body: &[u8]) -> bool {
    let head: Vec<u8> = body
        .iter()
        .take(64)
        .copied()
        .skip_while(|b| b.is_ascii_whitespace())
        .collect();
    head.first() == Some(&b'<')
}

/// GET (or POST a form) and return the response if it is a success.
pub(crate) async fn fetch(
    session: &Session,
    url: &Url,
    req: Req<'_>,
    site: &str,
) -> Result<Response, SocialError> {
    pace(url, req.min_gap).await;
    // Decided before sending: the response may add cookies of its own.
    let signed_in = req.cookies
        && req
            .login_cookie
            .is_some_and(|name| session.cookie(url, name).is_some());
    let client = session.client(&req.headers, req.cookies).await;
    let result = match &req.form {
        Some(body) => client.post_form(url, body).await,
        None => client.fetch(url).await,
    };
    let resp = result.map_err(|e| SocialError::Failed(format!("{site}: {e}")))?;
    match resp.status {
        200..=299 => Ok(resp),
        401 => Err(SocialError::LoginRequired(format!(
            "{site} rejected the credentials (HTTP 401); add or refresh the account or key"
        ))),
        403 if signed_in => Err(SocialError::AccountChallenge(format!(
            "{site} refused the signed-in request (HTTP 403); open {site} in a normal browser to check the account"
        ))),
        403 => Err(SocialError::Blocked(format!(
            "{site} refused anonymous access (HTTP 403); add an account or API key"
        ))),
        429 => Err(SocialError::RateLimited {
            retry_after_secs: retry_after(&resp),
            detail: format!("{site} rate limit"),
        }),
        402 => Err(SocialError::QuotaExceeded(format!("{site}: the plan's quota is used up (HTTP 402)"))),
        s => Err(SocialError::Failed(format!("{site} returned HTTP {s}"))),
    }
}

/// `fetch` and parse JSON. An HTML body (a block or login page served with
/// status 200) is reported as blocked rather than as a parse error.
pub(crate) async fn json(
    session: &Session,
    url: &Url,
    req: Req<'_>,
    site: &str,
) -> Result<Value, SocialError> {
    let resp = fetch(session, url, req, site).await?;
    if looks_like_html(&resp.body) {
        return Err(SocialError::Blocked(format!(
            "{site} answered with a web page instead of data"
        )));
    }
    serde_json::from_slice(&resp.body)
        .map_err(|e| SocialError::Failed(format!("{site}: unreadable JSON: {e}")))
}

pub(crate) fn url(s: &str) -> Result<Url, SocialError> {
    Url::parse(s).map_err(|e| SocialError::Failed(format!("bad request URL: {e}")))
}

/// Small JSON accessors used by every source.
pub(crate) fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

pub(crate) fn i(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

pub(crate) fn f(v: &Value, key: &str) -> Option<f64> {
    match v.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_detection() {
        assert!(looks_like_html(b"  \n<body class=theme-beta>"));
        assert!(!looks_like_html(b"{\"data\":1}"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pacing_spaces_requests_to_one_host() {
        let url = Url::parse("https://pace-test.invalid/").unwrap();
        let start = Instant::now();
        for _ in 0..3 {
            pace(&url, Duration::from_millis(100)).await;
        }
        assert!(
            start.elapsed() >= Duration::from_millis(200),
            "{:?}",
            start.elapsed()
        );
    }
}
