//! Recording, replay, trade logs and the report: the pieces that turn a feed into results.
use scalper::engine::{Variant, WindowEngine};
use scalper::market::{MarketMeta, Winner};
use scalper::record::{enforce_cap, format_line, parse_line, read_lines, Recorder};
use scalper::replay::{replay, Settled};
use scalper::report::{bootstrap_ci, summarize};
use scalper::trades::{read_trades, TradeLog};
use std::collections::HashMap;

const END: u64 = 1_000_000;

fn meta() -> MarketMeta {
    MarketMeta {
        slug: "btc-updown-5m-700".into(),
        asset: "btc".into(),
        dur_secs: 300,
        condition_id: "0xc".into(),
        up_token: "UP".into(),
        down_token: "DOWN".into(),
        start_ms: END - 300_000,
        end_ms: END,
        fee_rate: 0.07,
        min_size: 5.0,
    }
}

fn hold() -> Variant {
    Variant {
        name: "hold".into(),
        entry_secs: 15.0,
        min_secs: 0.0,
        min_ask: 0.90,
        max_ask: 0.99,
        stop: None,
        notional: 50.0,
        latency_ms: 200,
        max_slip: 0.02,
    }
}

fn up(bid: f64, ask: f64) -> String {
    format!(
        r#"{{"event_type":"book","asset_id":"UP","bids":[{{"price":"{bid}","size":"1000"}}],"asks":[{{"price":"{ask}","size":"1000"}}]}}"#
    )
}

fn feed() -> Vec<(u64, String)> {
    vec![(END - 10_000, up(0.90, 0.91)), (END - 9_700, up(0.90, 0.92)), (END - 2_000, up(0.97, 0.98))]
}

#[test]
fn line_round_trips_with_tabs_inside_the_frame() {
    let line = format_line(123, "slug-a", "{\"a\":\t1}");
    assert_eq!(parse_line(&line), Some((123, "slug-a", "{\"a\":\t1}")));
    assert_eq!(parse_line("garbage"), None);
}

#[test]
fn recorder_output_reads_back_and_survives_a_truncated_tail() {
    let dir = tempfile::tempdir().unwrap();
    let mut rec = Recorder::new(dir.path().join("raw")).unwrap();
    for i in 0..1000u64 {
        rec.write(3_600_000 + i, "s", &format!("{{\"n\":{i}}}")).unwrap();
    }
    rec.flush().unwrap();
    let files = rec.files().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(read_lines(&files[0]).unwrap().len(), 1000);
    // A crash mid-write leaves a cut-off file: keep what decodes.
    let bytes = std::fs::read(&files[0]).unwrap();
    std::fs::write(&files[0], &bytes[..bytes.len() - 3]).unwrap();
    assert!(read_lines(&files[0]).unwrap().len() < 1000);
}

#[test]
fn recorder_rotates_hourly() {
    let dir = tempfile::tempdir().unwrap();
    let mut rec = Recorder::new(dir.path().to_path_buf()).unwrap();
    rec.write(0, "s", "{}").unwrap();
    rec.write(3_600_000, "s", "{}").unwrap();
    rec.flush().unwrap();
    assert_eq!(rec.files().unwrap().len(), 2);
}

#[test]
fn replay_reproduces_the_live_engine_exactly() {
    let mut live = WindowEngine::new(meta(), vec![hold()]);
    for (t, f) in feed() {
        live.on_frame(t, &f).unwrap();
    }
    let live = live.finish(Some(Winner::Up)).trades;

    let dir = tempfile::tempdir().unwrap();
    let mut rec = Recorder::new(dir.path().to_path_buf()).unwrap();
    for (t, f) in feed() {
        rec.write(t, &meta().slug, &f).unwrap();
    }
    rec.flush().unwrap();
    let markets = HashMap::from([(meta().slug, Settled { meta: meta(), winner: Some(Winner::Up) })]);
    let reports = replay(&rec.files().unwrap(), &markets, &[hold()]).unwrap();
    let again = &reports[0].trades;
    assert_eq!(again.len(), live.len());
    assert_eq!(again[0].pnl_usd, live[0].pnl_usd);
    assert_eq!(again[0].entry_avg, live[0].entry_avg);
}

#[test]
fn replay_marks_recorded_disconnects() {
    let dir = tempfile::tempdir().unwrap();
    let mut rec = Recorder::new(dir.path().to_path_buf()).unwrap();
    let slug = meta().slug;
    rec.write(END - 10_000, &slug, &up(0.90, 0.91)).unwrap();
    rec.write(END - 9_700, &slug, &up(0.90, 0.91)).unwrap();
    rec.gap(END - 9_000, &slug).unwrap();
    rec.flush().unwrap();
    let markets = HashMap::from([(slug, Settled { meta: meta(), winner: Some(Winner::Up) })]);
    let r = replay(&rec.files().unwrap(), &markets, &[hold()]).unwrap();
    assert!(r[0].trades[0].tainted);
}

#[test]
fn trade_log_round_trips() {
    let mut e = WindowEngine::new(meta(), vec![hold()]);
    for (t, f) in feed() {
        e.on_frame(t, &f).unwrap();
    }
    let trades = e.finish(Some(Winner::Down)).trades;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trades.csv");
    TradeLog::open(&path).unwrap().append(&trades).unwrap();
    TradeLog::open(&path).unwrap().append(&trades).unwrap(); // header written once
    let back = read_trades(&path).unwrap();
    assert_eq!(back.len(), 2);
    assert_eq!(back[0].variant, "hold");
    assert!((back[0].pnl_usd - trades[0].pnl_usd).abs() < 1e-6);
    assert!(!back[0].won);
}

#[test]
fn bootstrap_ci_is_deterministic_and_brackets_the_mean() {
    let xs: Vec<f64> = (0..200).map(|i| if i % 10 == 0 { -90.0 } else { 9.0 }).collect();
    let mean = xs.iter().sum::<f64>() / xs.len() as f64;
    let (lo, hi) = bootstrap_ci(&xs, 2000);
    assert!(lo < mean && mean < hi, "{lo} {mean} {hi}");
    assert_eq!(bootstrap_ci(&xs, 2000), (lo, hi));
}

#[test]
fn summary_counts_only_clean_settled_fills() {
    let mut e = WindowEngine::new(meta(), vec![hold()]);
    for (t, f) in feed() {
        e.on_frame(t, &f).unwrap();
    }
    let mut trades = e.finish(Some(Winner::Up)).trades;
    let mut tainted = trades[0].clone();
    tainted.tainted = true;
    trades.push(tainted);
    let rows = summarize(&trades);
    let all = rows.iter().find(|r| r.series == "all").unwrap();
    assert_eq!(all.filled, 1);
    assert_eq!(all.tainted, 1);
    assert_eq!(all.wins, 1);
}

#[test]
fn disk_cap_deletes_oldest_files_first() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["2026110600.jsonl.zst", "2026110601.jsonl.zst", "2026110602.jsonl.zst"] {
        std::fs::write(dir.path().join(name), vec![0u8; 100]).unwrap();
    }
    let removed = enforce_cap(dir.path(), 250).unwrap();
    assert_eq!(removed, 1);
    assert!(!dir.path().join("2026110600.jsonl.zst").exists());
    assert!(dir.path().join("2026110602.jsonl.zst").exists());
}
