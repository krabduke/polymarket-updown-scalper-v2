//! Raw feed recording: one zstd file per UTC hour, one `ms<TAB>slug<TAB>frame` line per frame.
use anyhow::Result;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Frame text recorded when the connection drops, so replay can see the gap.
pub const GAP: &str = "#GAP";
/// Frame text recorded for a ping reply.
pub const ALIVE: &str = "#ALIVE";

pub fn format_line(ms: u64, slug: &str, frame: &str) -> String {
    format!("{ms}\t{slug}\t{}\n", frame.replace('\n', " "))
}

pub fn parse_line(line: &str) -> Option<(u64, &str, &str)> {
    let (ms, rest) = line.split_once('\t')?;
    let (slug, frame) = rest.split_once('\t')?;
    Some((ms.parse().ok()?, slug, frame.trim_end_matches('\n')))
}

fn hour_name(ms: u64) -> String {
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
    let secs = ms / 1000;
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}{:02}.jsonl.zst", (secs % 86_400) / 3600)
}

pub struct Recorder {
    dir: PathBuf,
    hour: Option<u64>,
    out: Option<zstd::Encoder<'static, File>>,
}

impl Recorder {
    pub fn new(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self { dir, hour: None, out: None })
    }

    pub fn write(&mut self, ms: u64, slug: &str, frame: &str) -> Result<()> {
        let hour = ms / 3_600_000;
        if self.hour != Some(hour) {
            if let Some(old) = self.out.take() {
                old.finish()?;
            }
            // Appending after a restart starts a new zstd frame, which decoders accept.
            let file = fs::OpenOptions::new().create(true).append(true).open(self.dir.join(hour_name(ms)))?;
            self.out = Some(zstd::Encoder::new(file, 3)?);
            self.hour = Some(hour);
        }
        let out = self.out.as_mut().expect("opened above");
        out.write_all(format_line(ms, slug, frame).as_bytes())?;
        Ok(())
    }

    pub fn gap(&mut self, ms: u64, slug: &str) -> Result<()> {
        self.write(ms, slug, GAP)
    }

    /// Makes everything written so far readable, even if the process dies next.
    pub fn flush(&mut self) -> Result<()> {
        if let Some(out) = self.out.as_mut() {
            out.flush()?;
        }
        Ok(())
    }

    pub fn files(&self) -> Result<Vec<PathBuf>> {
        list_raw(&self.dir)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(out) = self.out.take() {
            let _ = out.finish();
        }
    }
}

/// Raw files in time order (the names sort chronologically).
pub fn list_raw(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".jsonl.zst"))
        .collect();
    files.sort();
    Ok(files)
}

/// Every complete line that decodes. A truncated tail is dropped, not an error.
pub fn read_lines(path: &Path) -> Result<Vec<String>> {
    let reader = BufReader::new(zstd::Decoder::new(File::open(path)?)?);
    let mut lines = Vec::new();
    for line in reader.lines() {
        match line {
            Ok(l) => lines.push(l),
            Err(_) => break,
        }
    }
    Ok(lines)
}

/// Deletes the oldest raw files until the folder fits in `max_bytes`. Returns how many went.
pub fn enforce_cap(dir: &Path, max_bytes: u64) -> Result<usize> {
    let files = list_raw(dir)?;
    let sizes: Vec<u64> = files.iter().map(|f| fs::metadata(f).map(|m| m.len()).unwrap_or(0)).collect();
    let mut total: u64 = sizes.iter().sum();
    let mut removed = 0;
    // Never delete the newest file: it's the one being written.
    for (f, size) in files.iter().zip(&sizes).take(files.len().saturating_sub(1)) {
        if total <= max_bytes {
            break;
        }
        fs::remove_file(f)?;
        total -= size;
        removed += 1;
    }
    Ok(removed)
}
