//! `scalper run`     always-on paper forward test (records feeds, logs simulated trades)
//! `scalper replay`  backtest: rerun recorded feeds through any set of variants
//! `scalper report`  summary per variant and series with 95% intervals
use anyhow::{bail, Context, Result};
use scalper::engine::{default_variants, Variant};
use scalper::live::{run, Config};
use scalper::market::Series;
use scalper::record::list_raw;
use scalper::replay::{replay, Settled};
use scalper::report::{render, summarize};
use scalper::trades::{read_trades, TradeLog};
use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;

// musl's default allocator kept growing under this workload; see Memory in ANALYSIS.md.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const USAGE: &str = "usage:
  scalper run    [--data DIR] [--assets btc,eth,..] [--durations 60,300,900] [--watch-secs 90] [--raw-cap-gb 6] [--variants FILE]
  scalper replay [--data DIR] [--variants FILE] [--out FILE]
  scalper report [--data DIR] [--trades FILE]";

struct Args(Vec<String>);

impl Args {
    fn get(&self, flag: &str) -> Option<&str> {
        self.0.iter().position(|a| a == flag).and_then(|i| self.0.get(i + 1)).map(String::as_str)
    }
    fn data(&self) -> PathBuf {
        PathBuf::from(self.get("--data").unwrap_or("data"))
    }
    fn variants(&self) -> Result<Vec<Variant>> {
        match self.get("--variants") {
            Some(f) => Ok(serde_json::from_str(&std::fs::read_to_string(f)?).context("variants file")?),
            None => Ok(default_variants()),
        }
    }
}

fn load_markets(data: &std::path::Path) -> Result<HashMap<String, Settled>> {
    let file = std::fs::File::open(data.join("markets.jsonl")).context("no markets.jsonl yet")?;
    let mut out = HashMap::new();
    for line in std::io::BufReader::new(file).lines() {
        let s: Settled = serde_json::from_str(&line?)?;
        out.insert(s.meta.slug.clone(), s);
    }
    Ok(out)
}

#[tokio::main]
async fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = argv.first().cloned() else { bail!(USAGE) };
    let args = Args(argv);
    match cmd.as_str() {
        "run" => {
            let assets = args.get("--assets").unwrap_or("btc,eth,sol,xrp,doge,bnb,hype,zec");
            let durs = args.get("--durations").unwrap_or("60,300,900");
            let mut series = Vec::new();
            for d in durs.split(',') {
                for a in assets.split(',') {
                    series.push(Series::new(a.trim(), d.trim().parse().context("--durations")?));
                }
            }
            let gb: f64 = args.get("--raw-cap-gb").unwrap_or("6").parse()?;
            run(Config {
                data: args.data(),
                series,
                watch_secs: args.get("--watch-secs").unwrap_or("90").parse()?,
                raw_cap_bytes: (gb * 1e9) as u64,
                variants: args.variants()?,
            })
            .await
        }
        "replay" => {
            let data = args.data();
            let markets = load_markets(&data)?;
            let reports = replay(&list_raw(&data.join("raw"))?, &markets, &args.variants()?)?;
            let trades: Vec<_> = reports.into_iter().flat_map(|r| r.trades).collect();
            let out = PathBuf::from(args.get("--out").unwrap_or("replay_trades.csv"));
            if out.exists() {
                std::fs::remove_file(&out)?;
            }
            TradeLog::open(&out)?.append(&trades)?;
            print!("{}", render(&summarize(&trades)));
            eprintln!("{} trades written to {}", trades.len(), out.display());
            Ok(())
        }
        "report" => {
            let path = args.get("--trades").map(PathBuf::from).unwrap_or_else(|| args.data().join("trades.csv"));
            print!("{}", render(&summarize(&read_trades(&path)?)));
            Ok(())
        }
        _ => bail!(USAGE),
    }
}
