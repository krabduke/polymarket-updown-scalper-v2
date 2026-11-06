use std::collections::BTreeMap;

/// Prices are integers in units of 0.0001, the finest tick Polymarket uses.
const SCALE: f64 = 10_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

/// Parses a decimal price string without going through floats.
pub fn px(s: &str) -> Option<u32> {
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if frac.len() > 4 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut v = whole.parse::<u32>().ok()?.checked_mul(10_000)?;
    let mut unit = 1000;
    for b in frac.bytes() {
        v += u32::from(b - b'0') * unit;
        unit /= 10;
    }
    Some(v)
}

/// Result of walking one side of the book. `cost` is the notional in USDC.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Fill {
    pub shares: f64,
    pub cost: f64,
    pub fee: f64,
}

impl Fill {
    pub fn avg(&self) -> f64 {
        if self.shares > 0.0 { self.cost / self.shares } else { 0.0 }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Book {
    bids: BTreeMap<u32, f64>,
    asks: BTreeMap<u32, f64>,
}

impl Book {
    pub fn replace(&mut self, bids: &[(u32, f64)], asks: &[(u32, f64)]) {
        self.bids = bids.iter().copied().filter(|l| l.1 > 0.0).collect();
        self.asks = asks.iter().copied().filter(|l| l.1 > 0.0).collect();
    }

    pub fn set_level(&mut self, side: Side, price: u32, size: f64) {
        let levels = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        if size > 0.0 {
            levels.insert(price, size);
        } else {
            levels.remove(&price);
        }
    }

    pub fn clear(&mut self) {
        self.bids.clear();
        self.asks.clear();
    }

    pub fn best_bid(&self) -> Option<f64> {
        self.bids.keys().next_back().map(|p| f64::from(*p) / SCALE)
    }

    pub fn best_ask(&self) -> Option<f64> {
        self.asks.keys().next().map(|p| f64::from(*p) / SCALE)
    }

    /// Spends up to `budget` USDC on asks priced at or below `limit`.
    pub fn buy_usd(&self, budget: f64, limit: f64) -> Fill {
        let mut fill = Fill::default();
        let mut left = budget;
        for (p, size) in &self.asks {
            let price = f64::from(*p) / SCALE;
            if price > limit + 1e-9 || left <= 1e-9 {
                break;
            }
            let take = size.min(left / price);
            fill.shares += take;
            fill.cost += take * price;
            left -= take * price;
        }
        fill
    }

    /// Sells up to `shares` into the bids, best first, with no price floor.
    pub fn sell_shares(&self, shares: f64) -> Fill {
        self.sell_walk(shares, &mut BTreeMap::new())
    }

    /// A market sell that remembers what it took. `taken` holds the size this
    /// trader already sold into at each bid price; that size is gone, so a later
    /// sale can't hit it again even though the public feed still shows it.
    pub fn sell_walk(&self, shares: f64, taken: &mut BTreeMap<u32, f64>) -> Fill {
        let mut fill = Fill::default();
        let mut left = shares;
        for (p, size) in self.bids.iter().rev() {
            if left <= 1e-9 {
                break;
            }
            let used = taken.entry(*p).or_insert(0.0);
            let take = (size - *used).max(0.0).min(left);
            if take <= 0.0 {
                continue;
            }
            *used += take;
            fill.shares += take;
            fill.cost += take * f64::from(*p) / SCALE;
            left -= take;
        }
        fill
    }
}
