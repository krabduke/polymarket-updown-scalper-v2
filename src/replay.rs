//! Backtest: runs recorded raw frames back through the same engine the forward test uses.
use crate::engine::{Variant, WindowEngine, WindowReport};
use crate::market::{MarketMeta, Winner};
use crate::record::{parse_line, read_lines, ALIVE, GAP};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settled {
    pub meta: MarketMeta,
    pub winner: Option<Winner>,
}

pub fn replay(files: &[PathBuf], markets: &HashMap<String, Settled>, variants: &[Variant]) -> Result<Vec<WindowReport>> {
    let mut engines: HashMap<String, WindowEngine> = HashMap::new();
    let mut order = Vec::new();
    for file in files {
        for line in read_lines(file)? {
            let Some((ms, slug, frame)) = parse_line(&line) else { continue };
            let Some(settled) = markets.get(slug) else { continue };
            let engine = engines.entry(slug.to_string()).or_insert_with(|| {
                order.push(slug.to_string());
                WindowEngine::new(settled.meta.clone(), variants.to_vec())
            });
            if frame == GAP {
                engine.on_gap(ms);
            } else if frame == ALIVE {
                engine.on_heartbeat(ms);
            } else {
                let _ = engine.on_frame(ms, frame); // counted in the report
            }
        }
    }
    Ok(order
        .into_iter()
        .filter_map(|slug| {
            let engine = engines.remove(&slug)?;
            Some(engine.finish(markets[&slug].winner))
        })
        .collect())
}
