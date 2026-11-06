/// Taker fee in USDC: `shares × rate × p × (1 − p)`. Makers pay nothing.
pub fn taker_fee(shares: f64, price: f64, rate: f64) -> f64 {
    shares * rate * price * (1.0 - price)
}
