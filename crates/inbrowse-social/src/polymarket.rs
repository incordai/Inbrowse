//! Polymarket prediction markets through the public Gamma search API. One item
//! per event; its text lists each open market with the current "Yes" odds.
//! Closed, inactive and zero-liquidity markets are skipped. Called from `lib.rs`.

use serde_json::Value;

use crate::http::{self, f, s, Req};
use crate::{time, DateConfidence, Engagement, Item, SearchRequest, Session, SocialError};

const SITE: &str = "Polymarket";

/// `outcomes` and `outcomePrices` are JSON arrays encoded as strings.
fn string_array(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .unwrap_or_default()
}

fn market_line(m: &Value) -> Option<String> {
    if m.get("closed").and_then(Value::as_bool) == Some(true)
        || m.get("active").and_then(Value::as_bool) == Some(false)
    {
        return None;
    }
    let question = s(m, "question")?;
    let outcomes = string_array(m, "outcomes");
    let prices = string_array(m, "outcomePrices");
    let odds: Vec<String> = outcomes
        .iter()
        .zip(&prices)
        .filter_map(|(o, p)| {
            p.parse::<f64>()
                .ok()
                .map(|p| format!("{o} {:.0}%", p * 100.0))
        })
        .collect();
    Some(if odds.is_empty() {
        question
    } else {
        format!("{question} ({})", odds.join(", "))
    })
}

fn event_item(e: &Value) -> Option<Item> {
    if e.get("closed").and_then(Value::as_bool) == Some(true)
        || e.get("active").and_then(Value::as_bool) == Some(false)
    {
        return None;
    }
    let liquidity = f(e, "liquidity");
    if liquidity.unwrap_or(0.0) <= 0.0 {
        return None;
    }
    let slug = s(e, "slug")?;
    let lines: Vec<String> = e
        .get("markets")
        .and_then(Value::as_array)
        .map(|m| m.iter().filter_map(market_line).collect())
        .unwrap_or_default();
    let secs = s(e, "updatedAt").and_then(|d| time::parse_rfc3339(&d));
    Some(Item {
        source: "polymarket",
        id: s(e, "id").unwrap_or_else(|| slug.clone()),
        url: format!("https://polymarket.com/event/{slug}"),
        title: s(e, "title"),
        text: lines.join("\n"),
        author: None,
        community: None,
        published_at: secs.map(time::to_rfc3339),
        // The last update, not when the market was created.
        date_confidence: DateConfidence::Approximate,
        engagement: Engagement {
            volume: f(e, "volume1mo").or_else(|| f(e, "volume")),
            liquidity,
            comments: e.get("commentCount").and_then(Value::as_i64),
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
    _warnings: &mut Vec<String>,
) -> Result<Vec<Item>, SocialError> {
    let mut url = http::url("https://gamma-api.polymarket.com/public-search")?;
    url.query_pairs_mut()
        .append_pair("q", &req.query)
        .append_pair("events_status", "active")
        .append_pair("keep_closed_markets", "0")
        .append_pair("limit_per_type", &req.limit.min(50).to_string());
    let body = http::json(
        session,
        &url,
        Req {
            cookies: false,
            ..Req::default()
        },
        SITE,
    )
    .await?;
    Ok(body
        .get("events")
        .and_then(Value::as_array)
        .map(|events| events.iter().filter_map(event_item).collect())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_with_open_and_closed_markets() {
        let e: Value = serde_json::from_str(
            r#"{"id":"812886","slug":"texas","title":"Texas Senate","active":true,"closed":false,
                "liquidity":176607.6,"volume1mo":74711.9,"updatedAt":"2026-11-02T17:41:31.033755Z",
                "markets":[
                  {"question":"Dems sweep?","outcomes":"[\"Yes\", \"No\"]","outcomePrices":"[\"0.375\", \"0.625\"]","active":true,"closed":false},
                  {"question":"Old one?","closed":true}
                ]}"#,
        )
        .unwrap();
        let item = event_item(&e).unwrap();
        assert_eq!(item.url, "https://polymarket.com/event/texas");
        assert_eq!(item.text, "Dems sweep? (Yes 38%, No 62%)");
        assert_eq!(item.engagement.volume, Some(74711.9));
        let mut closed = e.clone();
        closed["closed"] = Value::Bool(true);
        assert!(event_item(&closed).is_none());
    }
}
