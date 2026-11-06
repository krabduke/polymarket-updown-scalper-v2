//! The always-on forward test: finds each window's market, records its feed,
//! runs the paper strategies on it, waits for the real settlement, logs results.
use crate::engine::{Variant, WindowEngine, WindowReport};
use crate::market::{parse_market, parse_outcome, MarketMeta, Series, Winner};
use crate::record::{enforce_cap, Recorder, ALIVE, GAP};
use crate::replay::Settled;
use crate::trades::TradeLog;
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_tungstenite::tungstenite::Message;

const GAMMA: &str = "https://gamma-api.polymarket.com/markets";
const WS: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
const PING_EVERY: Duration = Duration::from_secs(4);
const READ_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Clone)]
pub struct Config {
    pub data: PathBuf,
    pub series: Vec<Series>,
    /// Record and simulate the last this-many seconds of each window.
    pub watch_secs: u64,
    pub raw_cap_bytes: u64,
    pub variants: Vec<Variant>,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn log(msg: &str) {
    eprintln!("{} {msg}", now_ms() / 1000);
}

enum Rec {
    Line(u64, String, String),
    Flush,
}

struct Shared {
    cfg: Config,
    http: reqwest::Client,
    rec: mpsc::Sender<Rec>,
    trades: Mutex<TradeLog>,
    windows: Mutex<std::fs::File>,
    markets: Mutex<std::fs::File>,
    /// Series with no market (1-minute today), mapped to when to look again.
    missing: Mutex<HashMap<String, u64>>,
}

fn append_line(file: &Mutex<std::fs::File>, line: &str) -> Result<()> {
    let mut f = file.lock().map_err(|_| anyhow::anyhow!("lock poisoned"))?;
    writeln!(f, "{line}")?;
    f.flush()?;
    Ok(())
}

fn open_append(path: PathBuf, header: Option<&str>) -> Result<std::fs::File> {
    let fresh = !path.exists();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    if let (true, Some(h)) = (fresh, header) {
        writeln!(f, "{h}")?;
    }
    Ok(f)
}

pub async fn run(cfg: Config) -> Result<()> {
    std::fs::create_dir_all(&cfg.data)?;
    let lock = std::fs::File::create(cfg.data.join("scalper.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock).context("another instance is already running on this data folder")?;

    // One thread owns the recorder, so socket loops never touch the disk.
    let (tx, rx) = mpsc::channel::<Rec>();
    let mut recorder = Recorder::new(cfg.data.join("raw"))?;
    std::thread::spawn(move || {
        for msg in rx {
            let res = match msg {
                Rec::Line(ms, slug, frame) => recorder.write(ms, &slug, &frame),
                Rec::Flush => recorder.flush(),
            };
            if let Err(e) = res {
                log(&format!("recorder error: {e:#}"));
            }
        }
    });

    let shared = Arc::new(Shared {
        http: reqwest::Client::builder()
            .user_agent("updown-scalper-v2")
            .timeout(Duration::from_secs(10))
            .build()?,
        rec: tx,
        trades: Mutex::new(TradeLog::open(&cfg.data.join("trades.csv"))?),
        windows: Mutex::new(open_append(
            cfg.data.join("windows.csv"),
            Some("slug,asset,dur_secs,winner,frames,book_updates,gaps,parse_errors,best_price_mismatches,trades"),
        )?),
        markets: Mutex::new(open_append(cfg.data.join("markets.jsonl"), None)?),
        missing: Mutex::new(HashMap::new()),
        cfg,
    });
    log(&format!(
        "started: {} series, watching last {}s, {} variants",
        shared.cfg.series.len(),
        shared.cfg.watch_secs,
        shared.cfg.variants.len()
    ));

    let housekeeping = shared.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        let mut n = 0u64;
        loop {
            tick.tick().await;
            let _ = housekeeping.rec.send(Rec::Flush);
            n += 1;
            if n % 120 == 0 {
                match enforce_cap(&housekeeping.cfg.data.join("raw"), housekeeping.cfg.raw_cap_bytes) {
                    Ok(0) => {}
                    Ok(k) => log(&format!("disk cap: removed {k} oldest raw files")),
                    Err(e) => log(&format!("disk cap error: {e:#}")),
                }
            }
        }
    });

    let mut launched: HashSet<String> = HashSet::new();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tick.tick().await;
        let now_s = now_ms() / 1000;
        for series in &shared.cfg.series {
            let start = series.window_start(now_s);
            let end = start + series.dur_secs;
            let lead = shared.cfg.watch_secs.min(series.dur_secs) + 20;
            let slug = series.slug(start);
            let skip = shared.missing.lock().map(|m| m.get(&series.label()).is_some_and(|t| now_s < *t)).unwrap_or(false);
            if !skip && now_s + lead >= end && launched.insert(slug) {
                tokio::spawn(window(shared.clone(), series.clone(), start));
            }
        }
        if launched.len() > 1000 {
            launched.clear(); // only stale windows are lost; current ones are past launch time
            for series in &shared.cfg.series {
                launched.insert(series.slug(series.window_start(now_s)));
            }
        }
    }
}

async fn get_text(http: &reqwest::Client, url: &str) -> Result<String> {
    let resp = http.get(url).send().await?.error_for_status()?;
    Ok(resp.text().await?)
}

async fn fetch_meta(sh: &Shared, series: &Series, start: u64, deadline_ms: u64) -> Option<MarketMeta> {
    let url = format!("{GAMMA}?slug={}", series.slug(start));
    while now_ms() < deadline_ms {
        match get_text(&sh.http, &url).await.and_then(|b| parse_market(&b, series, start)) {
            Ok(Some(m)) => return Some(m),
            Ok(None) => {}
            Err(e) => log(&format!("{}: gamma error: {e:#}", series.slug(start))),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    None
}

async fn fetch_outcome(sh: &Shared, slug: &str) -> Option<Winner> {
    let url = format!("{GAMMA}?slug={slug}&closed=true");
    for _ in 0..180 { // up to an hour: resolution sometimes takes minutes
        tokio::time::sleep(Duration::from_secs(20)).await;
        match get_text(&sh.http, &url).await.and_then(|b| parse_outcome(&b)) {
            Ok(Some(w)) => return Some(w),
            Ok(None) => {}
            Err(e) => log(&format!("{slug}: outcome error: {e:#}")),
        }
    }
    None
}

async fn window(sh: Arc<Shared>, series: Series, start: u64) {
    let slug = series.slug(start);
    let end_ms = (start + series.dur_secs) * 1000;
    let watch_ms = sh.cfg.watch_secs.min(series.dur_secs) * 1000;
    let Some(meta) = fetch_meta(&sh, &series, start, end_ms - watch_ms / 2).await else {
        // 1-minute markets don't exist yet. Look again in 30 minutes, so the series
        // starts by itself if they launch, without hammering the API meanwhile.
        log(&format!("{slug}: no market found"));
        if series.dur_secs <= 60 {
            if let Ok(mut m) = sh.missing.lock() {
                m.insert(series.label(), now_ms() / 1000 + 1800);
            }
        }
        return;
    };
    let wait = (end_ms - watch_ms).saturating_sub(now_ms());
    tokio::time::sleep(Duration::from_millis(wait)).await;

    let mut engine = WindowEngine::new(meta.clone(), sh.cfg.variants.clone());
    if now_ms() > end_ms - watch_ms + 5_000 {
        // Started mid-window (after a restart): part of the watch period was missed.
        let ms = now_ms();
        let _ = sh.rec.send(Rec::Line(ms, slug.clone(), GAP.to_string()));
        engine.on_gap(ms);
    }
    // Hard ceiling, so one stuck socket can never hold a window forever.
    let budget = Duration::from_millis(end_ms + 10_000 - now_ms().min(end_ms));
    if tokio::time::timeout(budget, stream(&sh, &meta, &mut engine, end_ms + 3_000)).await.is_err() {
        log(&format!("{slug}: stream overran its time budget, cut off"));
    }

    let winner = fetch_outcome(&sh, &slug).await;
    let report = engine.finish(winner);
    let filled = report.trades.iter().filter(|t| t.status.as_str() == "filled").count();
    log(&format!(
        "{slug}: settled {:?}, {} frames, {} gaps, {} trades ({filled} filled)",
        winner,
        report.frames,
        report.gaps,
        report.trades.len()
    ));
    if let Err(e) = save(&sh, &meta, winner, &report) {
        log(&format!("{slug}: save error: {e:#}"));
    }
}

async fn stream(sh: &Shared, meta: &MarketMeta, engine: &mut WindowEngine, until_ms: u64) {
    let rec = |ms: u64, frame: &str| {
        let _ = sh.rec.send(Rec::Line(ms, meta.slug.clone(), frame.to_string()));
    };
    let sub = serde_json::json!({
        "assets_ids": [meta.up_token, meta.down_token],
        "type": "market",
        "custom_feature_enabled": true,
    })
    .to_string();
    let mut first = true;
    while now_ms() < until_ms {
        if !first {
            rec(now_ms(), GAP);
            engine.on_gap(now_ms());
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        first = false;
        let Ok(Ok((mut ws, _))) = tokio::time::timeout(Duration::from_secs(5), tokio_tungstenite::connect_async(WS)).await
        else {
            continue;
        };
        if ws.send(Message::text(sub.clone())).await.is_err() {
            continue;
        }
        let mut last_ping = std::time::Instant::now();
        loop {
            if now_ms() >= until_ms {
                // close() waits for the server's half of the handshake, which never
                // comes once we stop reading, so give it a second and drop the socket.
                let _ = tokio::time::timeout(Duration::from_secs(1), ws.close(None)).await;
                return;
            }
            if last_ping.elapsed() >= PING_EVERY {
                if ws.send(Message::text("PING")).await.is_err() {
                    break;
                }
                last_ping = std::time::Instant::now();
            }
            let left = Duration::from_millis(until_ms.saturating_sub(now_ms()));
            let wait = READ_TIMEOUT.min(PING_EVERY).min(left);
            match tokio::time::timeout(wait, ws.next()).await {
                // Quiet: the next pass pings. Only 12s with no reply at all is a gap.
                Err(_) => {}
                Ok(Some(Ok(Message::Text(t)))) => {
                    let ms = now_ms();
                    if t.as_str() == "PONG" {
                        rec(ms, ALIVE);
                        engine.on_heartbeat(ms);
                    } else {
                        rec(ms, t.as_str());
                        let _ = engine.on_frame(ms, t.as_str());
                    }
                }
                Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => break,
                Ok(Some(Ok(_))) => {}
            }
        }
    }
}

fn save(sh: &Shared, meta: &MarketMeta, winner: Option<Winner>, r: &WindowReport) -> Result<()> {
    sh.trades.lock().map_err(|_| anyhow::anyhow!("lock poisoned"))?.append(&r.trades)?;
    let w = match winner {
        Some(Winner::Up) => "up",
        Some(Winner::Down) => "down",
        None => "unknown",
    };
    append_line(
        &sh.windows,
        &format!(
            "{},{},{},{w},{},{},{},{},{},{}",
            meta.slug, meta.asset, meta.dur_secs, r.frames, r.book_updates, r.gaps, r.parse_errors,
            r.best_price_mismatches, r.trades.len()
        ),
    )?;
    append_line(&sh.markets, &serde_json::to_string(&Settled { meta: meta.clone(), winner })?)?;
    Ok(())
}
