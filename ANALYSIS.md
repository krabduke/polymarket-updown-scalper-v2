# Running it and analysing the results

## What is running

One process, `scalper run`, on the VPS in `~/updown-scalper/`:

- **Markets:** 24 series, which are 8 coins (btc, eth, sol, xrp, doge, bnb, hype, zec) × 3 durations (1m, 5m, 15m). Polymarket has no 1-minute markets today, so those 8 series check every 30 minutes and start on their own if they launch. In practice it covers **16 live series**.
- **Each window:** about 110 s before the close it finds the market on Gamma. 90 s before the close it opens a WebSocket for the Up and Down books and records every frame. It runs the paper variants on the live book, disconnects 3 s after the close, then polls Gamma (up to an hour) for the **real settlement** and writes the results.
- **Variants:** two, all paper, fixed before the test began. Both use the original entry (≤15 s left, ask 90–99¢); `stop80` market-sells at a bid of 80¢ or lower, and `hold` holds to settlement. See the README.
- **Resources:** a single static Rust binary run under `nice 10` / `ionice idle`. It uses well under 1% CPU on average and tens of MB of RAM.
- **Disk:** raw recordings are capped at 6 GB, with the oldest hours deleted first. The trade and window logs are tiny and are never deleted.

## Output files (`~/updown-scalper/data/`)

| File | One row per | Use |
|---|---|---|
| `trades.csv` | simulated trade (variant × window) | **The forward-test result.** Kept forever |
| `windows.csv` | recorded window | Data quality: frames, gaps, parse errors, best-price mismatches, settlement |
| `markets.jsonl` | recorded window | Market metadata plus settlement. Needed by `replay` |
| `raw/*.jsonl.zst` | UTC hour | The recorded feed, for backtesting new rules. Rolling window: at about 3.8 GB a day, the 6 GB cap keeps roughly the last 1.5 days |
| `../logs/scalper.log` | event | One line per settled window, plus any errors |

`trades.csv` columns:

| Column | Meaning |
|---|---|
| `variant` | which rule |
| `slug`, `asset`, `dur_secs` | which market |
| `side` | `up` or `down`, the side bought |
| `status` | `filled`; `failed_thin` (not enough depth inside the 2¢ slippage cap, or under 5 shares); `failed_too_late` (the order would land after the close); `unsettled` (no settlement from Gamma within an hour) |
| `secs_left` | seconds left when the rule triggered |
| `trigger_ask` / `entry_avg` | ask that triggered, and the average fill after walking the book 200 ms later |
| `shares` | shares bought for $50 |
| `stopped`, `exit_shares`, `exit_avg` | whether the stop fired, how much sold, at what average price |
| `fees` | taker fees in USD, entry plus exit |
| `won` | whether the side bought won at settlement |
| `pnl_usd`, `pnl_cents_per_share` | result after fees. **cents per share is the comparable number** |
| `tainted` | the feed dropped or went silent during the window, or the process started late. Excluded from reports |

## Analysing after two weeks

```bash
# from the Mac
scp -r user@your-vps:~/updown-scalper/data ./vps-data
cargo run --release -- report --data vps-data            # per variant and series, with 95% CIs
cargo run --release -- replay --data vps-data --variants v.json   # backtest new rules on the recorded hours
```

The report gives filled count, win %, stop %, EV in cents per share with a bootstrap 95% interval, profit factor, P&L and max drawdown. DuckDB or pandas read `trades.csv` directly.

**Rule decided in advance:** a variant counts as an edge only if the **lower** end of its 95% interval on EV per share is above **+1¢**, pooled across series, on untainted fills. Anything smaller sits inside the fill-model error (queue position and latency aren't modelled). Several variants are tested at once, so a single series clearing the bar on its own is a lead to recheck on fresh data, not a result.

**Expected sample in two weeks:** the 15 s rule triggers in only about 6% of 5-minute windows under TWAP settlement, which is roughly 2,000–2,500 signals across all coins. That's enough to rule out a large edge but not to confirm a small one.

**Check data quality first:** in `windows.csv`, gaps should be rare and `parse_errors` should be 0. A `best_price_mismatches` share above about 1% of `book_updates` means the book rebuild is drifting from the feed and needs a look.
