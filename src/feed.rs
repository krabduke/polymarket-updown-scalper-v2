//! Parser for the public market WebSocket channel. Borrows from the frame, so
//! a price_change costs no allocation beyond the event list.
use crate::book::{px, Side};
use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, PartialEq)]
pub enum Event<'a> {
    Book { asset: &'a str, bids: Vec<(u32, f64)>, asks: Vec<(u32, f64)> },
    Level { asset: &'a str, side: Side, price: u32, size: f64, best_bid: Option<u32>, best_ask: Option<u32> },
    Trade { asset: &'a str, price: u32, size: f64 },
    BestBidAsk { asset: &'a str },
    Other,
}

#[derive(Deserialize)]
struct RawLevel<'a> {
    #[serde(borrow)]
    price: &'a str,
    #[serde(borrow)]
    size: &'a str,
}

#[derive(Deserialize)]
struct RawChange<'a> {
    #[serde(borrow)]
    asset_id: &'a str,
    #[serde(borrow)]
    price: &'a str,
    #[serde(borrow)]
    size: &'a str,
    #[serde(borrow)]
    side: &'a str,
    #[serde(borrow, default)]
    best_bid: Option<&'a str>,
    #[serde(borrow, default)]
    best_ask: Option<&'a str>,
}

#[derive(Deserialize)]
struct RawEvent<'a> {
    #[serde(borrow, default)]
    event_type: Option<&'a str>,
    #[serde(borrow, default)]
    asset_id: Option<&'a str>,
    #[serde(borrow, default)]
    bids: Vec<RawLevel<'a>>,
    #[serde(borrow, default)]
    asks: Vec<RawLevel<'a>>,
    #[serde(borrow, default)]
    price_changes: Vec<RawChange<'a>>,
    #[serde(borrow, default)]
    price: Option<&'a str>,
    #[serde(borrow, default)]
    size: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawFrame<'a> {
    #[serde(borrow)]
    Many(Vec<RawEvent<'a>>),
    #[serde(borrow)]
    One(RawEvent<'a>),
}

fn levels(raw: &[RawLevel]) -> Result<Vec<(u32, f64)>> {
    raw.iter()
        .map(|l| Ok((px(l.price).context("bad price")?, l.size.parse()?)))
        .collect()
}

pub fn parse_frame(text: &str) -> Result<Vec<Event<'_>>> {
    let raw: RawFrame = serde_json::from_str(text)?;
    let events = match raw {
        RawFrame::Many(v) => v,
        RawFrame::One(e) => vec![e],
    };
    let mut out = Vec::with_capacity(events.len());
    for e in events {
        // price_change frames carry no event_type on some paths, so the nested
        // list decides. The old bot read these fields at the wrong depth.
        if !e.price_changes.is_empty() {
            for c in &e.price_changes {
                let side = match c.side {
                    "BUY" => Side::Buy,
                    "SELL" => Side::Sell,
                    other => bail!("unknown side {other}"),
                };
                out.push(Event::Level {
                    asset: c.asset_id,
                    side,
                    price: px(c.price).context("bad price")?,
                    size: c.size.parse()?,
                    best_bid: c.best_bid.and_then(px),
                    best_ask: c.best_ask.and_then(px),
                });
            }
            continue;
        }
        let asset = e.asset_id.unwrap_or("");
        out.push(match e.event_type {
            Some("book") => Event::Book { asset, bids: levels(&e.bids)?, asks: levels(&e.asks)? },
            Some("last_trade_price") => Event::Trade {
                asset,
                price: e.price.and_then(px).context("trade price")?,
                size: e.size.context("trade size")?.parse()?,
            },
            Some("best_bid_ask") => Event::BestBidAsk { asset },
            _ => Event::Other,
        });
    }
    Ok(out)
}
