//! GitHub issues and pull requests through the REST search API. Anonymous
//! calls work at a low rate; `credentials` with provider "github" (a token)
//! raises it. Issues and PRs are searched separately because the API refuses
//! an anonymous search that names neither. Called from `lib.rs`.

use serde_json::Value;
use url::Url;

use crate::http::{self, i, s, Req};
use crate::{
    time, Auth, Comment, Credentials, DateConfidence, Engagement, Item, SearchRequest, Session,
    SocialError, Sort,
};

const SITE: &str = "GitHub";

fn request<'a>(credentials: Option<&Credentials>) -> Result<(Req<'a>, Auth), SocialError> {
    let mut req = Req {
        headers: vec![
            ("Accept", "application/vnd.github+json".into()),
            ("X-GitHub-Api-Version", "2022-11-28".into()),
        ],
        cookies: false,
        ..Req::default()
    };
    match credentials {
        Some(c) if c.provider.eq_ignore_ascii_case("github") => {
            req.headers
                .push(("Authorization", format!("Bearer {}", c.key)));
            Ok((req, Auth::Api))
        }
        Some(c) => Err(SocialError::Invalid(format!(
            "credentials for {} cannot be used with GitHub",
            c.provider
        ))),
        None => Ok((req, Auth::Anonymous)),
    }
}

fn repo_name(v: &Value) -> Option<String> {
    s(v, "repository_url").and_then(|u| {
        u.strip_prefix("https://api.github.com/repos/")
            .map(String::from)
    })
}

fn to_item(v: &Value) -> Option<Item> {
    let url = s(v, "html_url")?;
    let secs = s(v, "created_at").and_then(|d| time::parse_rfc3339(&d));
    Some(Item {
        source: "github",
        id: url.clone(),
        url,
        title: s(v, "title"),
        text: s(v, "body").unwrap_or_default(),
        author: v.get("user").and_then(|u| s(u, "login")),
        community: repo_name(v),
        published_at: secs.map(time::to_rfc3339),
        date_confidence: if secs.is_some() {
            DateConfidence::Exact
        } else {
            DateConfidence::Unknown
        },
        engagement: Engagement {
            likes: v.get("reactions").and_then(|r| i(r, "total_count")),
            comments: i(v, "comments"),
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
    warnings: &mut Vec<String>,
) -> Result<(Vec<Item>, Auth), SocialError> {
    let mut items = Vec::new();
    let mut auth = Auth::Anonymous;
    for kind in ["is:issue", "is:pull-request"] {
        let (options, a) = request(req.credentials.as_ref())?;
        auth = a;
        let mut url = http::url("https://api.github.com/search/issues")?;
        let q = format!(
            "{} created:{}..{} {kind}",
            req.query,
            time::to_date(from),
            time::to_date(to)
        );
        url.query_pairs_mut()
            .append_pair("q", &q)
            .append_pair("per_page", &req.limit.min(100).to_string());
        match req.sort {
            Sort::Top => {
                url.query_pairs_mut()
                    .append_pair("sort", "reactions")
                    .append_pair("order", "desc");
            }
            Sort::Recent => {
                url.query_pairs_mut()
                    .append_pair("sort", "created")
                    .append_pair("order", "desc");
            }
            Sort::Relevance => {}
        }
        let body = http::json(session, &url, options, SITE).await?;
        if body.get("incomplete_results").and_then(Value::as_bool) == Some(true) {
            warnings
                .push("GitHub returned incomplete results (search timed out on their side)".into());
        }
        if let Some(list) = body.get("items").and_then(Value::as_array) {
            items.extend(list.iter().filter_map(to_item));
        }
    }
    Ok((items, auth))
}

/// `https://github.com/{owner}/{repo}/(issues|pull)/{n}`.
fn parse_thread_url(url: &Url) -> Result<(String, String, String), SocialError> {
    let parts: Vec<&str> = url
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    match parts.as_slice() {
        [owner, repo, "issues" | "pull", n, ..] if n.chars().all(|c| c.is_ascii_digit()) => {
            Ok((owner.to_string(), repo.to_string(), n.to_string()))
        }
        _ => Err(SocialError::Invalid(
            "expected https://github.com/<owner>/<repo>/issues/<n> or /pull/<n>".into(),
        )),
    }
}

pub(crate) async fn thread(
    session: &Session,
    url: &Url,
    comments: usize,
    credentials: Option<&Credentials>,
    _warnings: &mut Vec<String>,
) -> Result<(Item, Auth), SocialError> {
    let (owner, repo, n) = parse_thread_url(url)?;
    let base = format!("https://api.github.com/repos/{owner}/{repo}/issues/{n}");
    let (options, auth) = request(credentials)?;
    let issue = http::json(session, &http::url(&base)?, options, SITE).await?;
    let mut item =
        to_item(&issue).ok_or_else(|| SocialError::Failed("GitHub: issue without a URL".into()))?;
    if comments > 0 {
        let (options, _) = request(credentials)?;
        let list = http::json(
            session,
            &http::url(&format!("{base}/comments?per_page=100"))?,
            options,
            SITE,
        )
        .await?;
        let mut all: Vec<(i64, Comment)> = list
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| {
                        let reactions = c
                            .get("reactions")
                            .and_then(|r| i(r, "total_count"))
                            .unwrap_or(0);
                        (
                            reactions,
                            Comment {
                                author: c.get("user").and_then(|u| s(u, "login")),
                                text: s(c, "body").unwrap_or_default(),
                                score: Some(reactions),
                                published_at: s(c, "created_at"),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        all.sort_by(|a, b| b.0.cmp(&a.0));
        item.top_comments = all.into_iter().take(comments).map(|(_, c)| c).collect();
    }
    Ok((item, auth))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_urls() {
        let u =
            Url::parse("https://github.com/rust-lang/rust/issues/164250#issuecomment-1").unwrap();
        assert_eq!(
            parse_thread_url(&u).unwrap(),
            ("rust-lang".into(), "rust".into(), "164250".into())
        );
        assert!(
            parse_thread_url(&Url::parse("https://github.com/rust-lang/rust").unwrap()).is_err()
        );
    }

    #[test]
    fn issue_to_item() {
        let v: Value = serde_json::from_str(
            r#"{"html_url":"https://github.com/rust-lang/rust/issues/164250","title":"t","body":"b",
                "user":{"login":"jsnell"},"created_at":"2026-10-13T08:45:54Z","comments":19,
                "reactions":{"total_count":150},"repository_url":"https://api.github.com/repos/rust-lang/rust"}"#,
        )
        .unwrap();
        let item = to_item(&v).unwrap();
        assert_eq!(item.community.as_deref(), Some("rust-lang/rust"));
        assert_eq!(item.engagement.likes, Some(150));
        assert_eq!(item.author.as_deref(), Some("jsnell"));
    }
}
