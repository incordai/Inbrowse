//! Live checks against the real sites. Ignored by default so CI never depends
//! on them; run with:
//!   cargo test --release -p inbrowse-social --test live -- --ignored --test-threads=1
//! A Reddit app id/secret in INBROWSE_TEST_REDDIT_ID / INBROWSE_TEST_REDDIT_SECRET
//! also exercises the official API route.

use std::sync::Arc;

use inbrowse_net::{CookieJar, DEFAULT_USER_AGENT};
use inbrowse_social::{
    search, session, thread, Auth, Credentials, SearchRequest, Session, SocialError, Sort, Source,
};

fn sess() -> Session {
    session(
        Arc::new(CookieJar::new()),
        None,
        DEFAULT_USER_AGENT.to_string(),
    )
}

fn req(source: Source, query: &str) -> SearchRequest {
    SearchRequest {
        source,
        query: query.into(),
        days: 30,
        limit: 10,
        sort: Sort::Relevance,
        community: None,
        credentials: None,
    }
}

fn assert_items(results: &inbrowse_social::Results) {
    assert!(
        !results.items.is_empty(),
        "no items: {:?}",
        results.warnings
    );
    for item in &results.items {
        assert!(item.url.starts_with("https://"), "{}", item.url);
        assert!(item.title.is_some() || !item.text.is_empty());
    }
    let scores: Vec<f64> = results.items.iter().map(|i| i.rank_score).collect();
    let sorted = {
        let mut s = scores.clone();
        s.sort_by(|a, b| b.partial_cmp(a).unwrap());
        s
    };
    assert_eq!(scores, sorted, "relevance results are ranked");
}

#[tokio::test]
#[ignore]
async fn hackernews_search_and_thread() {
    let s = sess();
    let results = search(&s, &req(Source::HackerNews, "rust")).await.unwrap();
    assert_items(&results);
    assert_eq!(results.auth, Auth::Anonymous);
    let first = &results.items[0];
    assert!(first.engagement.score.is_some() && first.published_at.is_some());
    let t = thread(&s, &first.url, 5, None).await.unwrap();
    assert_eq!(t.items.len(), 1);
    assert!(t.items[0].top_comments.len() <= 5);
}

#[tokio::test]
#[ignore]
async fn github_search_and_thread() {
    let s = sess();
    let results = search(&s, &req(Source::Github, "borrow checker"))
        .await
        .unwrap();
    assert_items(&results);
    let issue = results
        .items
        .iter()
        .find(|i| i.engagement.comments.unwrap_or(0) > 0)
        .expect("an issue with comments");
    let t = thread(&s, &issue.url, 3, None).await.unwrap();
    assert!(!t.items[0].top_comments.is_empty());
}

#[tokio::test]
#[ignore]
async fn polymarket_search() {
    let results = search(&sess(), &req(Source::Polymarket, "election"))
        .await
        .unwrap();
    assert_items(&results);
    assert!(results
        .items
        .iter()
        .all(|i| i.engagement.liquidity.unwrap_or(0.0) > 0.0));
}

#[tokio::test]
#[ignore]
async fn reddit_anonymous_is_reported_as_blocked_or_works() {
    // Twice on one session: Reddit's block page sets cookies, and the second
    // call must still count as anonymous, not as a signed-in account problem.
    let s = sess();
    for _ in 0..2 {
        match search(&s, &req(Source::Reddit, "rust")).await {
            Ok(results) => {
                assert_eq!(results.auth, Auth::Anonymous);
                assert_items(&results);
            }
            Err(e) => assert!(matches!(e, SocialError::Blocked(_)), "{e}"),
        }
    }
}

#[tokio::test]
#[ignore]
async fn reddit_official_api() {
    let (Ok(id), Ok(secret)) = (
        std::env::var("INBROWSE_TEST_REDDIT_ID"),
        std::env::var("INBROWSE_TEST_REDDIT_SECRET"),
    ) else {
        eprintln!("skipped: no Reddit app credentials in the environment");
        return;
    };
    let mut r = req(Source::Reddit, "rust");
    r.credentials = Some(Credentials {
        kind: "api_key".into(),
        provider: "reddit".into(),
        key: id,
        secret: Some(secret.clone()),
    });
    let results = search(&sess(), &r).await.unwrap();
    assert_eq!(results.auth, Auth::Api);
    assert_items(&results);
    let rendered = serde_json::to_string(&results).unwrap();
    assert!(
        !rendered.contains(&secret),
        "the secret must never appear in output"
    );
}
