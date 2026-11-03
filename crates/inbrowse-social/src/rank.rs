//! Ranking and de-duplication for `lib.rs::search`.
//!
//! Engagement is log-scaled per count so one viral post does not drown the
//! rest, with per-source weights (the ones last30days-skill uses, MIT). The
//! final score mixes engagement with the source's own relevance order.
//! Duplicates: same normalised URL, or text at least 70% alike by word or
//! character-trigram Jaccard similarity; the higher-ranked copy is kept.

use std::collections::HashSet;

use crate::{Item, Sort, Source};

fn ln(v: Option<i64>) -> f64 {
    (v.unwrap_or(0).max(0) as f64).ln_1p()
}

fn lnf(v: Option<f64>) -> f64 {
    v.unwrap_or(0.0).max(0.0).ln_1p()
}

pub(crate) fn engagement(item: &Item, source: Source) -> f64 {
    let e = &item.engagement;
    match source {
        Source::Reddit => {
            0.50 * ln(e.score) + 0.35 * ln(e.comments) + 0.05 * e.upvote_ratio.unwrap_or(0.0) * 10.0
        }
        Source::HackerNews => 0.55 * ln(e.score) + 0.45 * ln(e.comments),
        Source::Github => 0.60 * ln(e.likes) + 0.40 * ln(e.comments),
        Source::Polymarket => 0.60 * lnf(e.volume) + 0.40 * lnf(e.liquidity),
        Source::X => 0.55 * ln(e.likes) + 0.25 * ln(e.reposts) + 0.15 * ln(e.comments) + 0.05 * ln(e.quotes),
    }
}

/// Score and sort. Items arrive in the source's relevance order.
pub(crate) fn rank(items: &mut [Item], source: Source, sort: Sort) {
    let n = items.len().max(1) as f64;
    let scores: Vec<f64> = items.iter().map(|i| engagement(i, source)).collect();
    let max = scores.iter().cloned().fold(0.0_f64, f64::max).max(1e-9);
    for (pos, (item, score)) in items.iter_mut().zip(scores).enumerate() {
        let relevance = 1.0 - pos as f64 / n;
        item.rank_score = ((0.6 * score / max + 0.4 * relevance) * 1000.0).round() / 1000.0;
    }
    match sort {
        Sort::Recent => items.sort_by(|a, b| b.published_secs.cmp(&a.published_secs)),
        Sort::Top => items.sort_by(|a, b| {
            engagement(b, source)
                .partial_cmp(&engagement(a, source))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        Sort::Relevance => items.sort_by(|a, b| b.rank_score.partial_cmp(&a.rank_score).unwrap_or(std::cmp::Ordering::Equal)),
    }
}

/// Lower-case host without `www.`/`old.`/`m.`, no fragment, no `utm_*`
/// parameters, no trailing slash.
pub(crate) fn normalize_url(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return raw.trim().to_ascii_lowercase();
    };
    url.set_fragment(None);
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !k.starts_with("utm_") && k != "ref" && k != "share_id")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(kept);
    }
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let host = host
        .trim_start_matches("www.")
        .trim_start_matches("old.")
        .trim_start_matches("m.")
        .to_string();
    let path = url.path().trim_end_matches('/').to_string();
    match url.query() {
        Some(q) => format!("{host}{path}?{q}"),
        None => format!("{host}{path}"),
    }
}

fn words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(str::to_lowercase)
        .collect()
}

fn trigrams(text: &str) -> HashSet<String> {
    let chars: Vec<char> = text.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

/// Plain Jaccard: |A ∩ B| / |A ∪ B|.
fn similarity(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    let union = a.len() + b.len() - inter;
    inter as f64 / union as f64
}

fn signature(item: &Item) -> String {
    let mut s = item.title.clone().unwrap_or_default();
    s.push(' ');
    s.extend(item.text.chars().take(300));
    s
}

/// Keep the first (highest-ranked) of each group of duplicates.
pub(crate) fn dedupe(items: &mut Vec<Item>) {
    let mut seen_urls = HashSet::new();
    let mut kept: Vec<(HashSet<String>, HashSet<String>)> = Vec::new();
    items.retain(|item| {
        if !seen_urls.insert(normalize_url(&item.url)) {
            return false;
        }
        let sig = signature(item);
        let (w, t) = (words(&sig), trigrams(&sig));
        // Very short texts ("Nice!") match too easily to compare.
        if w.len() >= 4 && kept.iter().any(|(kw, kt)| similarity(&w, kw).max(similarity(&t, kt)) >= 0.7) {
            return false;
        }
        kept.push((w, t));
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DateConfidence, Engagement};

    fn item(url: &str, title: &str, score: i64) -> Item {
        Item {
            source: "hackernews",
            id: url.into(),
            url: url.into(),
            title: Some(title.into()),
            text: String::new(),
            author: None,
            community: None,
            published_at: None,
            date_confidence: DateConfidence::Exact,
            engagement: Engagement {
                score: Some(score),
                ..Default::default()
            },
            rank_score: 0.0,
            top_comments: Vec::new(),
            published_secs: None,
        }
    }

    #[test]
    fn url_normalisation() {
        assert_eq!(
            normalize_url("https://www.Example.com/a/b/?utm_source=x&id=3#frag"),
            "example.com/a/b?id=3"
        );
        assert_eq!(normalize_url("https://old.reddit.com/r/rust/"), "reddit.com/r/rust");
    }

    #[test]
    fn dedupe_by_url_and_text() {
        let mut items = vec![
            item("https://a.com/1", "Rust borrow checker unsoundness found after eleven years", 10),
            item("https://www.a.com/1/?utm_medium=x", "something else entirely", 5),
            item("https://b.com/2", "Rust borrow checker unsoundness found after eleven years!", 3),
            item("https://c.com/3", "A different story about Go generics performance", 1),
        ];
        dedupe(&mut items);
        let urls: Vec<&str> = items.iter().map(|i| i.url.as_str()).collect();
        assert_eq!(urls, ["https://a.com/1", "https://c.com/3"]);
    }

    #[test]
    fn engagement_outranks_position_when_large() {
        let mut items = vec![item("https://a.com/1", "first", 1), item("https://a.com/2", "second", 5000)];
        rank(&mut items, Source::HackerNews, Sort::Relevance);
        assert_eq!(items[0].url, "https://a.com/2");
        assert!(items[0].rank_score > items[1].rank_score);
    }
}
