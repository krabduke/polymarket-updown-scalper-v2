//! Summary statistics with honest uncertainty: a bootstrap 95% interval on EV per share.
use crate::engine::{SimStatus, SimTrade};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct Row {
    pub variant: String,
    pub series: String,
    pub signals: usize,
    pub filled: usize,
    pub failed: usize,
    pub tainted: usize,
    pub wins: usize,
    pub stops: usize,
    pub ev_cents: f64,
    pub ci: (f64, f64),
    pub profit_factor: f64,
    pub pnl_usd: f64,
    pub max_drawdown_usd: f64,
}

/// Percentile bootstrap of the mean, with a fixed seed so reports are reproducible.
pub fn bootstrap_ci(xs: &[f64], iters: usize) -> (f64, f64) {
    if xs.len() < 2 {
        return (f64::NAN, f64::NAN);
    }
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let n = xs.len();
    let mut means: Vec<f64> = (0..iters)
        .map(|_| (0..n).map(|_| xs[(next() % n as u64) as usize]).sum::<f64>() / n as f64)
        .collect();
    means.sort_by(|a, b| a.total_cmp(b));
    (means[iters * 25 / 1000], means[iters * 975 / 1000])
}

fn row(variant: &str, series: &str, trades: &[&SimTrade]) -> Row {
    let mut r = Row { variant: variant.into(), series: series.into(), signals: trades.len(), ..Row::default() };
    let mut per_share = Vec::new();
    let (mut gain, mut loss, mut equity, mut peak) = (0.0, 0.0, 0.0f64, 0.0f64);
    for t in trades {
        if t.tainted {
            r.tainted += 1;
            continue;
        }
        match t.status {
            SimStatus::Filled => {}
            SimStatus::FailedThin | SimStatus::FailedTooLate => {
                r.failed += 1;
                continue;
            }
            SimStatus::Unsettled => continue,
        }
        r.filled += 1;
        r.wins += usize::from(t.won);
        r.stops += usize::from(t.stopped);
        per_share.push(t.pnl_cents_per_share);
        r.pnl_usd += t.pnl_usd;
        if t.pnl_usd >= 0.0 { gain += t.pnl_usd } else { loss -= t.pnl_usd }
        equity += t.pnl_usd;
        peak = peak.max(equity);
        r.max_drawdown_usd = r.max_drawdown_usd.max(peak - equity);
    }
    if !per_share.is_empty() {
        r.ev_cents = per_share.iter().sum::<f64>() / per_share.len() as f64;
    }
    r.ci = bootstrap_ci(&per_share, 2000);
    r.profit_factor = if loss > 0.0 { gain / loss } else { f64::INFINITY };
    r
}

/// One row per variant across everything ("all"), then one per variant and series.
pub fn summarize(trades: &[SimTrade]) -> Vec<Row> {
    let mut groups: BTreeMap<(String, String), Vec<&SimTrade>> = BTreeMap::new();
    for t in trades {
        let series = format!("{}-{}m", t.asset, t.dur_secs / 60);
        groups.entry((t.variant.clone(), "all".into())).or_default().push(t);
        groups.entry((t.variant.clone(), series)).or_default().push(t);
    }
    groups.iter().map(|((v, s), ts)| row(v, s, ts)).collect()
}

pub fn render(rows: &[Row]) -> String {
    let mut out = format!(
        "{:<14} {:<10} {:>6} {:>6} {:>6} {:>6} {:>6} {:>8} {:>20} {:>6} {:>10} {:>8}\n",
        "variant", "series", "filled", "failed", "taint", "win%", "stop%", "EV c/sh", "95% CI", "PF", "P&L $", "maxDD $"
    );
    for r in rows {
        let pct = |n: usize| if r.filled > 0 { 100.0 * n as f64 / r.filled as f64 } else { 0.0 };
        out += &format!(
            "{:<14} {:<10} {:>6} {:>6} {:>6} {:>6.1} {:>6.1} {:>8.2} {:>20} {:>6.2} {:>10.2} {:>8.2}\n",
            r.variant, r.series, r.filled, r.failed, r.tainted, pct(r.wins), pct(r.stops), r.ev_cents,
            format!("[{:.2}, {:.2}]", r.ci.0, r.ci.1), r.profit_factor, r.pnl_usd, r.max_drawdown_usd
        );
    }
    out
}
