//! Each test pins a hole found in the audit of the old bot, so it can't return.
use scalper::engine::{SimStatus, Variant, WindowEngine};
use scalper::market::{MarketMeta, Winner};

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

fn stop80() -> Variant {
    Variant {
        name: "t".into(),
        entry_secs: 15.0,
        min_secs: 0.0,
        min_ask: 0.90,
        max_ask: 0.99,
        stop: Some(0.80),
        notional: 50.0,
        latency_ms: 200,
        max_slip: 0.02,
    }
}

fn hold() -> Variant {
    Variant { stop: None, ..stop80() }
}

/// A book frame for the Up token.
fn up(bid: f64, ask: f64) -> String {
    format!(
        r#"{{"event_type":"book","asset_id":"UP","bids":[{{"price":"{bid}","size":"1000"}}],"asks":[{{"price":"{ask}","size":"1000"}}]}}"#
    )
}

fn engine(v: Variant) -> WindowEngine {
    WindowEngine::new(meta(), vec![v])
}

fn send(e: &mut WindowEngine, secs_left: f64, frame: &str) {
    e.on_frame(END - (secs_left * 1000.0) as u64, frame).unwrap();
}

#[test]
fn no_entry_before_the_entry_window() {
    let mut e = engine(hold());
    send(&mut e, 20.0, &up(0.91, 0.92));
    send(&mut e, 19.0, &up(0.91, 0.92));
    assert!(e.finish(Some(Winner::Up)).trades.is_empty());
}

#[test]
fn ask_below_the_threshold_does_not_trigger() {
    let mut e = engine(hold());
    send(&mut e, 10.0, &up(0.88, 0.89));
    send(&mut e, 9.0, &up(0.88, 0.89));
    assert!(e.finish(Some(Winner::Up)).trades.is_empty());
}

#[test]
fn entry_fills_at_the_real_ask_after_latency_not_at_the_threshold() {
    // The old "Case B" booked exactly 90c. Here the ask has moved on by the fill.
    let mut e = engine(hold());
    send(&mut e, 10.0, &up(0.90, 0.91));
    send(&mut e, 9.9, &up(0.91, 0.92)); // the book standing when the order lands at 9.8
    send(&mut e, 9.7, &up(0.92, 0.93));
    let t = &e.finish(Some(Winner::Up)).trades[0];
    assert_eq!(t.status, SimStatus::Filled);
    assert!((t.entry_avg - 0.92).abs() < 1e-9, "{}", t.entry_avg);
}

#[test]
fn ask_running_past_the_slippage_cap_fails_the_entry() {
    let mut e = engine(hold());
    send(&mut e, 10.0, &up(0.90, 0.91));
    send(&mut e, 9.9, &up(0.95, 0.96));
    send(&mut e, 9.7, &up(0.95, 0.96));
    let t = &e.finish(Some(Winner::Up)).trades[0];
    assert_eq!(t.status, SimStatus::FailedThin);
    assert_eq!(t.pnl_usd, 0.0);
}

#[test]
fn entry_that_cannot_fill_before_the_close_is_too_late() {
    let mut e = engine(hold());
    send(&mut e, 0.1, &up(0.90, 0.91));
    e.on_frame(END + 100, &up(0.98, 0.99)).unwrap();
    assert_eq!(e.finish(Some(Winner::Up)).trades[0].status, SimStatus::FailedTooLate);
}

#[test]
fn win_and_loss_come_from_real_settlement() {
    // The old bot booked every position that reached the close as a win.
    let run = |w| {
        let mut e = engine(hold());
        send(&mut e, 10.0, &up(0.90, 0.91));
        send(&mut e, 9.7, &up(0.90, 0.91));
        e.finish(Some(w)).trades.remove(0)
    };
    let won = run(Winner::Up);
    let lost = run(Winner::Down);
    let fee = 0.07 * 0.91 * 0.09;
    assert!((won.pnl_cents_per_share - (9.0 - fee * 100.0)).abs() < 1e-6, "{}", won.pnl_cents_per_share);
    assert!((lost.pnl_cents_per_share - (-91.0 - fee * 100.0)).abs() < 1e-6, "{}", lost.pnl_cents_per_share);
    assert!(won.won && !lost.won);
}

#[test]
fn stop_fills_at_the_gapped_bid_not_at_the_stop_price() {
    let mut e = engine(stop80());
    send(&mut e, 10.0, &up(0.90, 0.91));
    send(&mut e, 9.7, &up(0.90, 0.91));
    send(&mut e, 5.0, &up(0.79, 0.81)); // trigger
    send(&mut e, 4.9, &up(0.40, 0.45)); // book gapped during the latency
    send(&mut e, 4.7, &up(0.40, 0.45));
    let t = e.finish(Some(Winner::Up)).trades.remove(0);
    assert!(t.stopped);
    assert!((t.exit_avg - 0.40).abs() < 1e-9, "{}", t.exit_avg);
    let fees = 0.07 * 0.91 * 0.09 + 0.07 * 0.40 * 0.60;
    assert!((t.pnl_cents_per_share - (40.0 - 91.0 - fees * 100.0)).abs() < 1e-6);
}

#[test]
fn stop_with_no_bids_rides_to_settlement() {
    let empty = r#"{"event_type":"book","asset_id":"UP","bids":[],"asks":[{"price":"0.5","size":"10"}]}"#;
    let mut e = engine(stop80());
    send(&mut e, 10.0, &up(0.90, 0.91));
    send(&mut e, 9.7, &up(0.90, 0.91));
    send(&mut e, 5.0, empty);
    send(&mut e, 4.0, empty);
    let t = e.finish(Some(Winner::Down)).trades.remove(0);
    assert!(t.stopped);
    assert_eq!(t.exit_shares, 0.0);
    assert!(t.pnl_cents_per_share < -91.0);
}

#[test]
fn entry_below_the_minimum_order_size_fails() {
    let thin = r#"{"event_type":"book","asset_id":"UP","bids":[],"asks":[{"price":"0.91","size":"4"}]}"#;
    let mut e = engine(hold());
    send(&mut e, 10.0, thin);
    send(&mut e, 9.7, thin);
    assert_eq!(e.finish(Some(Winner::Up)).trades[0].status, SimStatus::FailedThin);
}

#[test]
fn only_one_trade_per_window_per_variant() {
    let mut e = engine(hold());
    for i in 0..50 {
        send(&mut e, 14.0 - f64::from(i) * 0.2, &up(0.90, 0.91));
    }
    assert_eq!(e.finish(Some(Winner::Up)).trades.len(), 1);
}

#[test]
fn feed_gap_taints_the_window_and_clears_books() {
    let mut e = engine(hold());
    send(&mut e, 10.0, &up(0.90, 0.91));
    send(&mut e, 9.7, &up(0.90, 0.91));
    e.on_gap(END - 9_000);
    let r = e.finish(Some(Winner::Up));
    assert_eq!(r.gaps, 1);
    assert!(r.trades[0].tainted);
}

#[test]
fn silence_while_watching_counts_as_a_gap() {
    let mut e = engine(hold());
    send(&mut e, 14.0, &up(0.90, 0.91));
    send(&mut e, 13.7, &up(0.90, 0.91));
    send(&mut e, 1.0, &up(0.90, 0.91)); // 12.7s of nothing
    let r = e.finish(Some(Winner::Up));
    assert!(r.trades[0].tainted);
    assert_eq!(r.gaps, 1);
}

#[test]
fn missing_settlement_is_reported_unsettled_not_guessed() {
    let mut e = engine(hold());
    send(&mut e, 10.0, &up(0.90, 0.91));
    send(&mut e, 9.7, &up(0.90, 0.91));
    let t = e.finish(None).trades.remove(0);
    assert_eq!(t.status, SimStatus::Unsettled);
    assert_eq!(t.pnl_usd, 0.0);
}

#[test]
fn all_variants_see_the_same_feed_independently() {
    let mut e = WindowEngine::new(meta(), vec![stop80(), hold()]);
    send(&mut e, 10.0, &up(0.90, 0.91));
    send(&mut e, 9.7, &up(0.90, 0.91));
    send(&mut e, 5.0, &up(0.79, 0.81));
    send(&mut e, 4.7, &up(0.79, 0.81));
    let r = e.finish(Some(Winner::Up));
    assert!(r.trades[0].stopped && r.trades[0].pnl_usd < 0.0);
    assert!(!r.trades[1].stopped && r.trades[1].pnl_usd > 0.0);
}

#[test]
fn malformed_frame_is_counted_not_fatal() {
    let mut e = engine(hold());
    assert!(e.on_frame(END - 10_000, "{nonsense").is_err());
    assert_eq!(e.finish(None).parse_errors, 1);
}

#[test]
fn heartbeats_keep_a_quiet_market_clean() {
    let mut e = engine(hold());
    send(&mut e, 14.0, &up(0.90, 0.91));
    send(&mut e, 13.7, &up(0.90, 0.91));
    for s in [10.0, 6.0, 2.0] {
        e.on_heartbeat(END - (s * 1000.0) as u64);
    }
    send(&mut e, 1.0, &up(0.90, 0.91));
    let r = e.finish(Some(Winner::Up));
    assert_eq!(r.gaps, 0);
    assert!(!r.trades[0].tainted);
}

#[test]
fn stop_sale_can_trigger_on_a_heartbeat_after_the_latency() {
    // The exit's latency clock must not wait for the next book change.
    let mut e = engine(stop80());
    send(&mut e, 10.0, &up(0.90, 0.91));
    send(&mut e, 9.7, &up(0.90, 0.91));
    send(&mut e, 5.0, &up(0.79, 0.81));
    e.on_heartbeat(END - 4_000);
    let t = e.finish(Some(Winner::Up)).trades.remove(0);
    assert!((t.exit_shares - t.shares).abs() < 1e-9);
    assert!((t.exit_avg - 0.79).abs() < 1e-9);
}

#[test]
fn order_due_before_the_close_fills_against_the_closing_book() {
    // No frame after the trigger: the book didn't change, so the order still lands.
    let mut e = engine(hold());
    send(&mut e, 1.0, &up(0.90, 0.91));
    let t = e.finish(Some(Winner::Up)).trades.remove(0);
    assert_eq!(t.status, SimStatus::Filled);
    assert!((t.entry_avg - 0.91).abs() < 1e-9);
}

#[test]
fn v2_runs_exactly_the_original_entry_with_two_exits() {
    let vs = scalper::engine::default_variants();
    let names: Vec<&str> = vs.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["stop80", "hold"]);
    for v in &vs {
        assert_eq!((v.entry_secs, v.min_secs, v.min_ask, v.max_ask), (15.0, 0.0, 0.90, 0.99));
        assert_eq!(v.latency_ms, 200);
    }
    assert_eq!(vs[0].stop, Some(0.80));
    assert_eq!(vs[1].stop, None);
}

/// Up book with explicit bid levels and one ask.
fn up_bids(bids: &[(f64, f64)], ask: f64) -> String {
    let b: Vec<String> = bids.iter().map(|(p, s)| format!(r#"{{"price":"{p}","size":"{s}"}}"#)).collect();
    format!(
        r#"{{"event_type":"book","asset_id":"UP","bids":[{}],"asks":[{{"price":"{ask}","size":"1000"}}]}}"#,
        b.join(",")
    )
}

#[test]
fn market_sell_walks_every_bid_level_it_needs() {
    // About 54.9 shares to sell: 20 at 0.80, 20 at 0.75, the rest at 0.70.
    let book = up_bids(&[(0.80, 20.0), (0.75, 20.0), (0.70, 500.0)], 0.82);
    let mut e = engine(stop80());
    send(&mut e, 10.0, &up(0.90, 0.91));
    send(&mut e, 9.7, &up(0.90, 0.91));
    send(&mut e, 5.0, &book);
    send(&mut e, 4.7, &book);
    let t = e.finish(Some(Winner::Up)).trades.remove(0);
    let rest = t.shares - 40.0;
    let expect = (20.0 * 0.80 + 20.0 * 0.75 + rest * 0.70) / t.shares;
    assert!((t.exit_shares - t.shares).abs() < 1e-9);
    assert!((t.exit_avg - expect).abs() < 1e-9, "{} vs {expect}", t.exit_avg);
}

#[test]
fn our_own_sale_removes_the_bids_it_hit() {
    // Only 30 shares of bids exist. The retry must not sell into those same
    // 30 shares again just because the feed still shows them.
    let thin = up_bids(&[(0.80, 30.0)], 0.82);
    let mut e = engine(stop80());
    send(&mut e, 10.0, &up(0.90, 0.91));
    send(&mut e, 9.7, &up(0.90, 0.91));
    send(&mut e, 5.0, &thin);
    for s in [4.7, 4.4, 4.1, 3.8, 3.5] {
        send(&mut e, s, &thin);
    }
    let t = e.finish(Some(Winner::Down)).trades.remove(0);
    assert!((t.exit_shares - 30.0).abs() < 1e-9, "sold {}", t.exit_shares);
}

#[test]
fn remainder_sells_into_new_bids_after_another_delay() {
    let thin = up_bids(&[(0.80, 30.0)], 0.82);
    let refilled = up_bids(&[(0.80, 30.0), (0.60, 500.0)], 0.82);
    let mut e = engine(stop80());
    send(&mut e, 10.0, &up(0.90, 0.91));
    send(&mut e, 9.7, &up(0.90, 0.91));
    send(&mut e, 5.0, &thin);
    send(&mut e, 4.7, &thin); // first sale: 30 at 0.80
    send(&mut e, 4.6, &refilled); // new 0.60 bids, but the retry is still in flight
    send(&mut e, 4.4, &refilled); // retry lands: the rest at 0.60
    let t = e.finish(Some(Winner::Down)).trades.remove(0);
    let expect = (30.0 * 0.80 + (t.shares - 30.0) * 0.60) / t.shares;
    assert!((t.exit_shares - t.shares).abs() < 1e-9);
    assert!((t.exit_avg - expect).abs() < 1e-9, "{} vs {expect}", t.exit_avg);
}

#[test]
fn bid_above_80_never_triggers_the_stop() {
    let mut e = engine(stop80());
    send(&mut e, 10.0, &up(0.90, 0.91));
    send(&mut e, 9.7, &up(0.90, 0.91));
    send(&mut e, 5.0, &up(0.81, 0.83));
    send(&mut e, 2.0, &up(0.81, 0.83));
    let t = e.finish(Some(Winner::Up)).trades.remove(0);
    assert!(!t.stopped);
    assert!(t.pnl_usd > 0.0);
}
