//! Paper-only forward tester and replay backtester for Polymarket crypto
//! Up/Down markets. Nothing in this crate can place an order or hold a key.
pub mod book;
pub mod engine;
pub mod fee;
pub mod live;
pub mod feed;
pub mod market;
pub mod record;
pub mod replay;
pub mod report;
pub mod trades;
