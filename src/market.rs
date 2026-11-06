//! Series naming and Gamma API parsing.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Series {
    pub asset: String,
    pub dur_secs: u64,
}

impl Series {
    pub fn new(asset: &str, dur_secs: u64) -> Self {
        Self { asset: asset.to_string(), dur_secs }
    }

    pub fn label(&self) -> String {
        format!("{}-{}m", self.asset, self.dur_secs / 60)
    }

    pub fn window_start(&self, now_s: u64) -> u64 {
        now_s - now_s % self.dur_secs
    }

    pub fn slug(&self, start_s: u64) -> String {
        format!("{}-updown-{}m-{}", self.asset, self.dur_secs / 60, start_s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarketMeta {
    pub slug: String,
    pub asset: String,
    pub dur_secs: u64,
    pub condition_id: String,
    pub up_token: String,
    pub down_token: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub fee_rate: f64,
    pub min_size: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Winner {
    Up,
    Down,
}

/// Gamma returns raw control characters inside strings, which strict JSON
/// parsers reject. They carry no meaning here, so they become spaces.
fn clean(text: &str) -> String {
    text.chars().map(|c| if c < ' ' { ' ' } else { c }).collect()
}

fn string_list(v: &serde_json::Value, key: &str) -> Result<Vec<String>> {
    let s = v.get(key).and_then(|x| x.as_str()).with_context(|| format!("missing {key}"))?;
    Ok(serde_json::from_str(s)?)
}

pub fn parse_market(body: &str, series: &Series, start_s: u64) -> Result<Option<MarketMeta>> {
    let list: Vec<serde_json::Value> = serde_json::from_str(&clean(body))?;
    let Some(m) = list.first() else { return Ok(None) };
    let outcomes = string_list(m, "outcomes")?;
    let tokens = string_list(m, "clobTokenIds")?;
    let token = |name: &str| {
        outcomes
            .iter()
            .position(|o| o.eq_ignore_ascii_case(name))
            .and_then(|i| tokens.get(i).cloned())
            .with_context(|| format!("no {name} token"))
    };
    Ok(Some(MarketMeta {
        slug: series.slug(start_s),
        asset: series.asset.clone(),
        dur_secs: series.dur_secs,
        condition_id: m.get("conditionId").and_then(|x| x.as_str()).context("conditionId")?.to_string(),
        up_token: token("Up")?,
        down_token: token("Down")?,
        start_ms: start_s * 1000,
        end_ms: (start_s + series.dur_secs) * 1000,
        fee_rate: m
            .pointer("/feeSchedule/rate")
            .and_then(|x| x.as_f64())
            .context("feeSchedule.rate")?,
        min_size: m.get("orderMinSize").and_then(|x| x.as_f64()).context("orderMinSize")?,
    }))
}

/// The real settlement, or `None` until Gamma says closed with exact 1/0 prices.
pub fn parse_outcome(body: &str) -> Result<Option<Winner>> {
    let list: Vec<serde_json::Value> = serde_json::from_str(&clean(body))?;
    let Some(m) = list.first() else { return Ok(None) };
    if m.get("closed").and_then(|x| x.as_bool()) != Some(true) {
        return Ok(None);
    }
    let outcomes = string_list(m, "outcomes")?;
    let prices = string_list(m, "outcomePrices")?;
    let winner_at = prices.iter().position(|p| p.parse::<f64>() == Ok(1.0));
    let settled = prices.iter().filter(|p| p.parse::<f64>() == Ok(0.0)).count() == prices.len() - 1;
    let (Some(i), true) = (winner_at, settled) else { return Ok(None) };
    Ok(match outcomes.get(i).map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("up") => Some(Winner::Up),
        Some("down") => Some(Winner::Down),
        _ => None,
    })
}
