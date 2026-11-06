//! The trade log: plain CSV, one row per simulated trade.
use crate::engine::{SimStatus, SimTrade};
use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

const HEADER: &str = "variant,slug,asset,dur_secs,side,status,secs_left,trigger_ask,entry_avg,shares,stopped,exit_shares,exit_avg,fees,won,pnl_usd,pnl_cents_per_share,tainted";

pub struct TradeLog {
    file: File,
}

impl TradeLog {
    pub fn open(path: &Path) -> Result<Self> {
        let fresh = !path.exists() || std::fs::metadata(path)?.len() == 0;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        if fresh {
            writeln!(file, "{HEADER}")?;
        }
        Ok(Self { file })
    }

    pub fn append(&mut self, trades: &[SimTrade]) -> Result<()> {
        for t in trades {
            writeln!(
                self.file,
                "{},{},{},{},{},{},{:.3},{:.4},{:.6},{:.4},{},{:.4},{:.6},{:.6},{},{:.6},{:.6},{}",
                t.variant, t.slug, t.asset, t.dur_secs, t.side, t.status.as_str(), t.secs_left, t.trigger_ask,
                t.entry_avg, t.shares, t.stopped, t.exit_shares, t.exit_avg, t.fees, t.won, t.pnl_usd,
                t.pnl_cents_per_share, t.tainted
            )?;
        }
        self.file.flush()?;
        Ok(())
    }
}

pub fn read_trades(path: &Path) -> Result<Vec<SimTrade>> {
    let mut out = Vec::new();
    for (i, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if i == 0 || line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        let get = |n: usize| f.get(n).copied().with_context(|| format!("row {i}: missing column {n}"));
        let num = |n: usize| -> Result<f64> { Ok(get(n)?.parse()?) };
        let status = match get(5)? {
            "filled" => SimStatus::Filled,
            "failed_thin" => SimStatus::FailedThin,
            "failed_too_late" => SimStatus::FailedTooLate,
            _ => SimStatus::Unsettled,
        };
        out.push(SimTrade {
            variant: get(0)?.to_string(),
            slug: get(1)?.to_string(),
            asset: get(2)?.to_string(),
            dur_secs: get(3)?.parse()?,
            side: if get(4)? == "up" { "up" } else { "down" },
            status,
            secs_left: num(6)?,
            trigger_ask: num(7)?,
            entry_avg: num(8)?,
            shares: num(9)?,
            stopped: get(10)? == "true",
            exit_shares: num(11)?,
            exit_avg: num(12)?,
            fees: num(13)?,
            won: get(14)? == "true",
            pnl_usd: num(15)?,
            pnl_cents_per_share: num(16)?,
            tainted: get(17)? == "true",
        });
    }
    Ok(out)
}
