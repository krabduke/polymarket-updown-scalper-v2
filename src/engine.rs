//! One market window: books, plus one simulated trader per variant. Live and
//! replay both drive this same code, so a backtest can't drift from the forward test.
use crate::book::{Book, Fill};
use crate::fee::taker_fee;
use crate::feed::{parse_frame, Event};
use crate::market::{MarketMeta, Winner};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Silence longer than this during a window counts as a feed gap. The live
/// runner pings every 4s and passes replies to `on_heartbeat`, so a quiet but
/// healthy market never trips this.
pub const MAX_SILENCE_MS: u64 = 12_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variant {
    pub name: String,
    /// Enter only with at most this many seconds left.
    pub entry_secs: f64,
    /// ...and at least this many.
    pub min_secs: f64,
    pub min_ask: f64,
    pub max_ask: f64,
    /// Sell when the best bid reaches this. `None` holds to settlement.
    pub stop: Option<f64>,
    pub notional: f64,
    /// Decision-to-match delay, for the taker delay plus the network.
    pub latency_ms: u64,
    /// The order's limit price is the triggering ask plus this.
    pub max_slip: f64,
}

/// The variants fixed in advance for the forward test. All use real asks, real
/// book depth, a 200ms delay (150ms taker delay + 50ms network) and real settlement.
pub fn default_variants() -> Vec<Variant> {
    let base = Variant {
        name: String::new(),
        entry_secs: 15.0,
        min_secs: 0.0,
        min_ask: 0.90,
        max_ask: 0.99,
        stop: Some(0.80),
        notional: 50.0,
        latency_ms: 200,
        max_slip: 0.02,
    };
    vec![
        Variant { name: "old_stop80".into(), ..base.clone() },
        Variant { name: "old_hold".into(), stop: None, ..base.clone() },
        Variant { name: "tuned_stop80".into(), min_secs: 3.0, max_ask: 0.97, ..base.clone() },
        Variant { name: "tuned_hold".into(), min_secs: 3.0, max_ask: 0.97, stop: None, ..base.clone() },
        // Under TWAP most 5m windows are pinned near 100c by 15s left, so the
        // 15s rule fires rarely. This one widens the window to get a usable sample.
        Variant { name: "wide60_hold".into(), entry_secs: 60.0, min_secs: 3.0, max_ask: 0.97, stop: None, ..base },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimStatus {
    Filled,
    FailedThin,
    FailedTooLate,
    Unsettled,
}

impl SimStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SimStatus::Filled => "filled",
            SimStatus::FailedThin => "failed_thin",
            SimStatus::FailedTooLate => "failed_too_late",
            SimStatus::Unsettled => "unsettled",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SimTrade {
    pub variant: String,
    pub slug: String,
    pub asset: String,
    pub dur_secs: u64,
    pub side: &'static str,
    pub status: SimStatus,
    pub secs_left: f64,
    pub trigger_ask: f64,
    pub entry_avg: f64,
    pub shares: f64,
    pub stopped: bool,
    pub exit_shares: f64,
    pub exit_avg: f64,
    pub fees: f64,
    pub won: bool,
    pub pnl_usd: f64,
    pub pnl_cents_per_share: f64,
    pub tainted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy)]
enum State {
    Watching,
    PendingEntry { token: Token, ask: f64, secs_left: f64, fill_at: u64 },
    Holding,
    PendingExit { fill_at: u64 },
    Failed(SimStatus),
}

struct Sim {
    v: Variant,
    state: State,
    token: Token,
    secs_left: f64,
    trigger_ask: f64,
    entry: Fill,
    exit: Fill,
    stopped: bool,
    tainted: bool,
}

impl Sim {
    fn new(v: Variant) -> Self {
        Self {
            v,
            state: State::Watching,
            token: Token::Up,
            secs_left: 0.0,
            trigger_ask: 0.0,
            entry: Fill::default(),
            exit: Fill::default(),
            stopped: false,
            tainted: false,
        }
    }

    fn on_tick(&mut self, now: u64, meta: &MarketMeta, up: &Book, down: &Book) {
        let open = now < meta.end_ms;
        let secs_left = meta.end_ms.saturating_sub(now) as f64 / 1000.0;
        let book = |t: Token| if t == Token::Up { up } else { down };
        match self.state {
            State::Watching => {
                if !open || secs_left > self.v.entry_secs || secs_left < self.v.min_secs {
                    return;
                }
                for token in [Token::Up, Token::Down] {
                    if let Some(ask) = book(token).best_ask() {
                        if ask >= self.v.min_ask - 1e-9 && ask <= self.v.max_ask + 1e-9 {
                            self.state = State::PendingEntry {
                                token,
                                ask,
                                secs_left,
                                fill_at: now + self.v.latency_ms,
                            };
                            return;
                        }
                    }
                }
            }
            State::PendingEntry { token, ask, secs_left: at, fill_at } => {
                if now < fill_at {
                    return;
                }
                self.token = token;
                self.secs_left = at;
                self.trigger_ask = ask;
                if !open {
                    self.state = State::Failed(SimStatus::FailedTooLate);
                    return;
                }
                let mut fill = book(token).buy_usd(self.v.notional, ask + self.v.max_slip);
                if fill.shares < meta.min_size {
                    self.state = State::Failed(SimStatus::FailedThin);
                    return;
                }
                fill.fee = taker_fee(fill.shares, fill.avg(), meta.fee_rate);
                self.entry = fill;
                self.state = State::Holding;
            }
            State::Holding => {
                let Some(stop) = self.v.stop else { return };
                if open && book(self.token).best_bid().is_none_or(|b| b <= stop + 1e-9) {
                    self.stopped = true;
                    self.state = State::PendingExit { fill_at: now + self.v.latency_ms };
                }
            }
            State::PendingExit { fill_at } => {
                if now < fill_at || !open {
                    return;
                }
                let left = self.entry.shares - self.exit.shares;
                let fill = book(self.token).sell_shares(left);
                self.exit.shares += fill.shares;
                self.exit.cost += fill.cost;
                self.exit.fee += taker_fee(fill.shares, fill.avg(), meta.fee_rate);
            }
            State::Failed(_) => {}
        }
    }

    fn into_trade(self, meta: &MarketMeta, winner: Option<Winner>) -> Option<SimTrade> {
        let side = if self.token == Token::Up { "up" } else { "down" };
        let mut t = SimTrade {
            variant: self.v.name.clone(),
            slug: meta.slug.clone(),
            asset: meta.asset.clone(),
            dur_secs: meta.dur_secs,
            side,
            status: SimStatus::Filled,
            secs_left: self.secs_left,
            trigger_ask: self.trigger_ask,
            entry_avg: self.entry.avg(),
            shares: self.entry.shares,
            stopped: self.stopped,
            exit_shares: self.exit.shares,
            exit_avg: self.exit.avg(),
            fees: self.entry.fee + self.exit.fee,
            won: false,
            pnl_usd: 0.0,
            pnl_cents_per_share: 0.0,
            tainted: self.tainted,
        };
        match self.state {
            State::Watching => return None,
            // Decided but never reached the book before the feed ended.
            State::PendingEntry { token, ask, secs_left, .. } => {
                t.side = if token == Token::Up { "up" } else { "down" };
                t.trigger_ask = ask;
                t.secs_left = secs_left;
                t.status = SimStatus::FailedTooLate;
                return Some(t);
            }
            State::Failed(status) => {
                t.status = status;
                return Some(t);
            }
            State::Holding | State::PendingExit { .. } => {}
        }
        let Some(winner) = winner else {
            t.status = SimStatus::Unsettled;
            return Some(t);
        };
        t.won = matches!((self.token, winner), (Token::Up, Winner::Up) | (Token::Down, Winner::Down));
        let held = self.entry.shares - self.exit.shares;
        let payout = if t.won { held } else { 0.0 };
        t.pnl_usd = payout + self.exit.cost - self.entry.cost - t.fees;
        t.pnl_cents_per_share = t.pnl_usd / self.entry.shares * 100.0;
        Some(t)
    }
}

#[derive(Debug, Default, Clone)]
pub struct WindowReport {
    pub trades: Vec<SimTrade>,
    pub frames: u64,
    pub gaps: u32,
    pub parse_errors: u32,
    pub best_price_mismatches: u64,
    pub book_updates: u64,
}

pub struct WindowEngine {
    meta: MarketMeta,
    up: Book,
    down: Book,
    sims: Vec<Sim>,
    last_ms: Option<u64>,
    report: WindowReport,
}

impl WindowEngine {
    pub fn new(meta: MarketMeta, variants: Vec<Variant>) -> Self {
        Self {
            meta,
            up: Book::default(),
            down: Book::default(),
            sims: variants.into_iter().map(Sim::new).collect(),
            last_ms: None,
            report: WindowReport::default(),
        }
    }

    pub fn meta(&self) -> &MarketMeta {
        &self.meta
    }

    fn touch(&mut self, now_ms: u64) {
        if let Some(last) = self.last_ms {
            if now_ms.saturating_sub(last) > MAX_SILENCE_MS && last < self.meta.end_ms {
                self.mark_gap();
            }
        }
        self.last_ms = Some(now_ms);
    }

    /// A ping reply: the connection is alive even if the market is quiet.
    pub fn on_heartbeat(&mut self, now_ms: u64) {
        self.touch(now_ms);
        self.tick(now_ms);
    }

    /// Feeds one raw frame. A bad frame is counted and returned, and never stops the window.
    pub fn on_frame(&mut self, now_ms: u64, text: &str) -> Result<()> {
        self.report.frames += 1;
        self.touch(now_ms);
        // Orders whose latency ran out before this frame arrived hit the book as
        // it stood until now, so settle them first, then apply the frame.
        self.tick(now_ms);
        let events = parse_frame(text).inspect_err(|_| self.report.parse_errors += 1)?;
        for ev in events {
            self.apply(ev);
        }
        self.tick(now_ms);
        Ok(())
    }

    fn tick(&mut self, now_ms: u64) {
        for sim in &mut self.sims {
            sim.on_tick(now_ms, &self.meta, &self.up, &self.down);
        }
    }

    fn book_mut(&mut self, asset: &str) -> Option<&mut Book> {
        if asset == self.meta.up_token {
            Some(&mut self.up)
        } else if asset == self.meta.down_token {
            Some(&mut self.down)
        } else {
            None
        }
    }

    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Book { asset, bids, asks } => {
                if let Some(b) = self.book_mut(asset) {
                    b.replace(&bids, &asks);
                    self.report.book_updates += 1;
                }
            }
            Event::Level { asset, side, price, size, best_bid, best_ask } => {
                if let Some(b) = self.book_mut(asset) {
                    b.set_level(side, price, size);
                    let differs = |theirs: Option<u32>, ours: Option<f64>| {
                        matches!((theirs, ours), (Some(t), Some(o)) if (f64::from(t) / 10_000.0 - o).abs() > 1e-9)
                    };
                    let mismatch = differs(best_bid, b.best_bid()) || differs(best_ask, b.best_ask());
                    self.report.book_updates += 1;
                    self.report.best_price_mismatches += u64::from(mismatch);
                }
            }
            Event::Trade { .. } | Event::BestBidAsk { .. } | Event::Other => {}
        }
    }

    /// The connection dropped: the books can't be trusted until a fresh snapshot.
    pub fn on_gap(&mut self, _now_ms: u64) {
        self.up.clear();
        self.down.clear();
        self.mark_gap();
    }

    fn mark_gap(&mut self) {
        self.report.gaps += 1;
        for sim in &mut self.sims {
            sim.tainted = true;
        }
    }

    pub fn finish(mut self, winner: Option<Winner>) -> WindowReport {
        // Orders still waiting on latency meet the closing book if they were due before the close.
        let last = self.meta.end_ms - 1;
        if self.last_ms.is_some_and(|t| t < last) {
            for sim in &mut self.sims {
                if matches!(sim.state, State::PendingEntry { .. } | State::PendingExit { .. }) {
                    sim.on_tick(last, &self.meta, &self.up, &self.down);
                }
            }
        }
        let meta = self.meta;
        self.report.trades = self.sims.into_iter().filter_map(|s| s.into_trade(&meta, winner)).collect();
        self.report
    }
}
