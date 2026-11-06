# Polymarket Up/Down Scalper V2

A Rust rewrite of [polymarket-btc-scalper](https://github.com/krabduke/polymarket-btc-scalper): an always-on **paper** forward tester and replay backtester for Polymarket's crypto Up/Down markets.

**It cannot trade.** It has no wallet, no keys and no order code. It reads public market data, simulates what each strategy would have done against the real order book, and checks the result against the real settlement.

## Why a V2

An audit of V1 found that its headline profit came from simulation errors, not from the market:

| V1 hole | Effect | V2 fix |
|---|---|---|
| "Case B" booked buys at exactly 90¢ after the price had already crossed 90¢ | 38.7% of BTC trades, 128% of BTC's logged profit | Entries fill only against the asks actually in the book |
| Entry triggered on the mid, filled with no delay | Fills at prices that were gone | Trigger on the **ask**. The order lands 200 ms later (150 ms taker delay plus network), against the book standing at that moment |
| Every position that reached the close was booked as a win | Losses counted as wins | P&L uses the **real settlement** from Gamma. With no settlement, a trade is `unsettled`, never guessed |
| Stop exits assumed to fill at exactly 80¢ ("ideal") | About 89% of real stop exits filled below 80¢ | The stop sells into the real bids after the delay. Gaps and empty books are kept |
| Feed parser read `price_change` fields at the wrong depth | Book updates silently dropped | Nested `price_changes` parsed and tested against frames captured from the live feed |
| No record of feed outages (V1 ran on a laptop) | Missing data was invisible | Disconnects and 12 s silences mark the window `tainted`, and reports leave tainted trades out |
| Old fee rate (0.0625) | Costs understated | Fee read per market from `feeSchedule` (0.07 today): `shares × rate × p × (1−p)` |
| Fixed $50 size regardless of depth | Fills that didn't exist | The book is walked level by level, with a 2¢ slippage cap and the market's 5-share minimum |
| US-hours filter picked from the same backtest | In-sample overfit | Removed. Results are reported per series, so any time effect has to show up out of sample |

The research behind this (platform rules, the V1 audit, alternatives) lives in the parent project's `research_notes/`.

## What gets tested

Five variants run side by side on every window. They are fixed in advance, so the forward test isn't tuned on its own results:

| Variant | Entry | Exit |
|---|---|---|
| `old_stop80` | ≤15 s left, ask 90–99¢ | Sell when the bid reaches 80¢ |
| `old_hold` | same | Hold to settlement |
| `tuned_stop80` | 3–15 s left, ask 90–97¢ | Sell when the bid reaches 80¢ |
| `tuned_hold` | same | Hold to settlement |
| `wide60_hold` | 3–60 s left, ask 90–97¢ | Hold to settlement |

The tuned versions drop the last 3 seconds, when a fill is least likely to land, and skip asks of 98–99¢, which pay at most 2¢ against a full loss. `wide60_hold` exists because under TWAP settlement about 95% of 5-minute windows are already pinned near 100¢ with 15 seconds left, so the 15-second rule fires in only about 6% of windows. More variants can be backtested from the recorded data without touching the live run.

**Markets:** BTC, ETH, SOL, XRP, DOGE, BNB, HYPE and ZEC, in 5-minute and 15-minute windows. Polymarket has **no 1-minute Up/Down markets** today. The 1-minute series stays configured, so it starts recording on its own if they launch.

**Settlement:** since August 2026 these markets settle on a 60-second Chainlink **TWAP**, not a single closing price. Results from V1 don't carry over.

## Commands

```bash
cargo test                       # 39 tests: fees, books, feed parsing, strategy holes, replay parity
cargo build --release

scalper run    --data data       # forward test (records the last 90 s of every window)
scalper report --data data       # summary per variant and series, with 95% bootstrap intervals
scalper replay --data data --variants my_variants.json   # backtest other rules on the recorded feed
```

`replay` drives the same engine as `run`, so a backtest on recorded data reproduces the live paper results exactly (this is tested).

Output in `data/`:

- `trades.csv`: one row per simulated trade, with fill prices, fees, settlement, P&L and taint flag
- `windows.csv`: per window, the frames received, gaps, parse errors and any disagreement between our book and the feed's best prices
- `markets.jsonl`: market metadata plus settlement, used by `replay`
- `raw/YYYYMMDDHH.jsonl.zst`: the recorded feed. Disk use is capped (`--raw-cap-gb`, default 6) by deleting the oldest hours.

## Running on a server

```bash
deploy/deploy.sh user@host       # cross-compiles a static Linux binary, installs it, checks the checksum
```

On the server, a cron watchdog keeps it running, at low CPU and I/O priority:

```
* * * * * ~/updown-scalper/ensure_running.sh
@reboot sleep 60 && ~/updown-scalper/ensure_running.sh
```

## Limits

- Paper fills assume our order would have been matched against the visible book. They can't show queue position, hidden competition for the same liquidity, or our own market impact.
- The 200 ms delay is an estimate: the 150 ms taker delay is documented, but it can't be measured without placing orders.
- Settlement comes from Gamma's `outcomePrices` once the market is closed.
- Placing orders from some countries is restricted by Polymarket's terms. This tool only reads public data. Check eligibility before ever building order placement on it.

## License

MIT. Research and educational material, not financial advice.
