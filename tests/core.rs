//! Fees, books, feed parsing and Gamma parsing, against real captured frames.
use scalper::book::{px, Book, Side};
use scalper::feed::{parse_frame, Event};
use scalper::fee::taker_fee;
use scalper::market::{parse_market, parse_outcome, Series, Winner};

const FRAMES: &str = include_str!("fixtures/frames.txt");
const GAMMA: &str = include_str!("fixtures/gamma_live.json");

#[test]
fn fee_matches_published_crypto_table() {
    // Polymarket's table: 100 shares at $0.90 pays $0.63; at $0.50 pays $1.75.
    assert!((taker_fee(100.0, 0.90, 0.07) - 0.63).abs() < 1e-9);
    assert!((taker_fee(100.0, 0.50, 0.07) - 1.75).abs() < 1e-9);
}

#[test]
fn price_strings_parse_exactly() {
    assert_eq!(px("0.92"), Some(9200));
    assert_eq!(px("0.1"), Some(1000));
    assert_eq!(px("1"), Some(10000));
    assert_eq!(px("0.001"), Some(10));
    assert_eq!(px("0.9995"), Some(9995));
    assert_eq!(px("abc"), None);
}

fn sample_book() -> Book {
    let mut b = Book::default();
    b.replace(&[(8900, 100.0), (8800, 50.0)], &[(9000, 10.0), (9100, 1000.0)]);
    b
}

#[test]
fn best_prices_come_from_the_correct_ends() {
    let b = sample_book();
    assert_eq!(b.best_bid(), Some(0.89));
    assert_eq!(b.best_ask(), Some(0.90));
}

#[test]
fn zero_size_removes_a_level() {
    let mut b = sample_book();
    b.set_level(Side::Buy, 8900, 0.0);
    assert_eq!(b.best_bid(), Some(0.88));
}

#[test]
fn buy_walks_past_thin_top_of_book() {
    // $50 budget: 10 shares at 0.90 ($9), the other $41 at 0.91.
    let f = sample_book().buy_usd(50.0, 1.0);
    assert!((f.cost - 50.0).abs() < 1e-9);
    assert!((f.shares - (10.0 + 41.0 / 0.91)).abs() < 1e-9);
    assert!(f.avg() > 0.90 && f.avg() < 0.91);
}

#[test]
fn buy_stops_at_its_limit_price() {
    let f = sample_book().buy_usd(50.0, 0.90);
    assert!((f.shares - 10.0).abs() < 1e-9);
    assert!((f.cost - 9.0).abs() < 1e-9);
}

#[test]
fn sell_walks_down_and_reports_unfilled_remainder() {
    let f = sample_book().sell_shares(200.0);
    assert!((f.shares - 150.0).abs() < 1e-9);
    assert!((f.cost - (100.0 * 0.89 + 50.0 * 0.88)).abs() < 1e-9);
}

#[test]
fn real_frames_parse_with_nothing_unrecognised() {
    let mut seen = [0usize; 4];
    for line in FRAMES.lines().filter(|l| !l.trim().is_empty() && *l != "PONG") {
        for ev in parse_frame(line).unwrap_or_else(|e| panic!("{e}: {line}")) {
            match ev {
                Event::Book { .. } => seen[0] += 1,
                Event::Level { .. } => seen[1] += 1,
                Event::Trade { .. } => seen[2] += 1,
                Event::BestBidAsk { .. } => seen[3] += 1,
                Event::Other => {}
            }
        }
    }
    assert!(seen.iter().all(|n| *n > 0), "missing event type: {seen:?}");
}

#[test]
fn price_change_exposes_nested_levels() {
    // The old Python parser read these fields at the top level and silently lost them.
    let line = FRAMES.lines().find(|l| l.contains("price_changes")).unwrap();
    let evs = parse_frame(line).unwrap();
    let Event::Level { asset, price, side, .. } = &evs[0] else { panic!("{evs:?}") };
    assert!(asset.starts_with("1399553"));
    assert_eq!(*price, 400);
    assert_eq!(*side, Side::Buy);
}

#[test]
fn gamma_with_raw_control_characters_parses() {
    let m = parse_market(GAMMA, &Series::new("btc", 300), 1793989500).unwrap().unwrap();
    assert_eq!(m.slug, "btc-updown-5m-1793989500");
    assert!(m.up_token.starts_with("1150208"));
    assert!(m.down_token.starts_with("1399553"));
    assert_eq!(m.end_ms, (1793989500u64 + 300) * 1000);
    assert!((m.fee_rate - 0.07).abs() < 1e-12);
    assert_eq!(m.min_size, 5.0);
}

#[test]
fn slug_uses_window_start_and_duration() {
    let s = Series::new("eth", 900);
    assert_eq!(s.window_start(1793989999), 1793989800);
    assert_eq!(s.slug(1793989800), "eth-updown-15m-1793989800");
    assert_eq!(Series::new("btc", 60).slug(60), "btc-updown-1m-60");
}

#[test]
fn outcome_only_counts_when_closed_and_exactly_final() {
    let j = |closed: bool, p: &str| {
        format!(r#"[{{"closed":{closed},"outcomes":"[\"Up\",\"Down\"]","outcomePrices":"{p}"}}]"#)
    };
    assert_eq!(parse_outcome(&j(true, r#"[\"1\",\"0\"]"#)).unwrap(), Some(Winner::Up));
    assert_eq!(parse_outcome(&j(true, r#"[\"0\",\"1\"]"#)).unwrap(), Some(Winner::Down));
    assert_eq!(parse_outcome(&j(false, r#"[\"1\",\"0\"]"#)).unwrap(), None);
    assert_eq!(parse_outcome(&j(true, r#"[\"0.99\",\"0.01\"]"#)).unwrap(), None);
    assert_eq!(parse_outcome("[]").unwrap(), None);
}
