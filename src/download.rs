//! Concurrent, checksummed download engine with optional HTTP byte-range splitting.
use crate::{error::{classify, ErrorCategory}, validate_package, Package};
use anyhow::{bail, Context, Result};
use futures_util::{stream, StreamExt};
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use reqwest::{header, Client, StatusCode};
use sha2::{Digest, Sha256};
use std::{io::IsTerminal, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicU64, Ordering}}, time::Instant};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
};

#[derive(Default)]
struct Counters {
    bytes: AtomicU64,
    requests: AtomicU64,
    range_parts: AtomicU64,
    retries: AtomicU64,
    cache_hits: AtomicU64,
    verified: AtomicU64,
}

fn progress_bar(bar: ProgressBar, name: &str) -> ProgressBar {
    // Never draw before MultiProgress owns the bar: the initial "queued"
    // line would otherwise become an orphan that cannot be overwritten.
    bar.set_prefix(name.chars().take(22).collect::<String>());
    bar.set_style(ProgressStyle::with_template(
        "{spinner:.cyan} {prefix:22} [{bar:28.cyan/blue}] {bytes:>10}/{total_bytes:10} {bytes_per_sec:>11} {eta:>5} {msg}"
    ).expect("valid progress template").progress_chars("=>-"));
    bar.set_message("queued");
    bar
}

#[derive(Clone)]
pub struct Downloader {
    client: Client,
    pub package_workers: usize,
    pub segments: usize,
    counters: Arc<Counters>,
}

impl Downloader {
    pub fn new(package_workers: usize, segments: usize) -> Result<Self> {
        if package_workers == 0 || segments == 0 || package_workers > 64 || segments > 16 {
            bail!("workers must be 1..=64 and segments 1..=16");
        }
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()?;
        Ok(Self { client, package_workers, segments, counters: Arc::new(Counters::default()) })
    }

    pub async fn download_all(&self, packages: Vec<Package>, dest: &Path) -> Result<()> {
        let started = Instant::now();
        let runner = Self {
            client: self.client.clone(),
            package_workers: self.package_workers,
            segments: self.segments,
            counters: Arc::new(Counters::default()),
        };
        let count = packages.len();
        let multi = MultiProgress::new();
        if !std::io::stderr().is_terminal() {
            multi.set_draw_target(ProgressDrawTarget::hidden());
        }
        // The set of bars is fixed during this fetch. Cursor movement lets
        // progress updates replace the same terminal rows instead of appending.
        multi.set_move_cursor(true);
        let bars = packages.iter()
            .map(|p| progress_bar(multi.add(ProgressBar::new(p.size)), &p.name))
            .collect::<Vec<_>>();
        // Retain a handle for each completed bar to prevent it becoming a
        // zombie and shifting/redrawing the remaining rows.
        let results = stream::iter(packages.into_iter().zip(bars.iter().cloned()).map(|(package, bar)| {
            let this = runner.clone();
            let root = dest.to_path_buf();
            async move {
                let result = this.download_one_with_bar(&package, &root, &bar).await;
                if result.is_err() { bar.abandon_with_message("failed"); }
                (package.name, result)
            }
        }))
        .buffer_unordered(self.package_workers)
        .collect::<Vec<_>>().await;
        let c = &runner.counters;
        let elapsed = started.elapsed().as_secs_f64();
        let bytes = c.bytes.load(Ordering::Relaxed);
        let failed = results.iter().filter(|(_, result)| result.is_err()).count();
        println!("Download summary:");
        println!("  Packages: {}/{} verified ({} failed, {} cached)",
            c.verified.load(Ordering::Relaxed), count, failed, c.cache_hits.load(Ordering::Relaxed));
        println!("  Transferred: {:.2} MiB", bytes as f64 / 1_048_576.0);
        println!("  HTTP requests: {} ({} range parts)",
            c.requests.load(Ordering::Relaxed), c.range_parts.load(Ordering::Relaxed));
        println!("  Retries: {}", c.retries.load(Ordering::Relaxed));
        println!("  Elapsed: {:.2}s | Throughput: {:.2} MiB/s",
            elapsed, (bytes as f64 / 1_048_576.0) / elapsed.max(0.000001));
        // Emit one machine-readable category prefix per failed package on stderr.
        let mut has_system_error = false;
        for (name, result) in results {
            if let Err(error) = result {
                let category = classify(&error, ErrorCategory::Download);
                has_system_error |= category == ErrorCategory::System;
                eprintln!("rpt: error [{category}] package {name}: {error:#}");
            }
        }
        if failed > 0 {
            return Err(anyhow::anyhow!("{failed} of {count} package(s) failed")
                .context(if has_system_error { ErrorCategory::System } else { ErrorCategory::Download }));
        }
        Ok(())
    }

    pub async fn download_one(&self, package: &Package, dest: &Path) -> Result<PathBuf> {
        self.download_one_with_bar(package, dest, &ProgressBar::hidden()).await
    }

    async fn download_one_with_bar(
        &self, package: &Package, dest: &Path, bar: &ProgressBar,
    ) -> Result<PathBuf> {
        validate_package(package).context(ErrorCategory::System)?;
        let target = dest.join(&package.filename);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).await.context(ErrorCategory::System)?;
        }
        if fs::try_exists(&target).await.context(ErrorCategory::System)? && verify(&target, &package.sha256).await.context(ErrorCategory::System)? {
            self.counters.cache_hits.fetch_add(1, Ordering::Relaxed);
            self.counters.verified.fetch_add(1, Ordering::Relaxed);
            bar.set_position(package.size);
            bar.finish_with_message("cached");
            return Ok(target);
        }
        let temp = target.with_extension(format!("rpt-{}.part", std::process::id()));
        let mut attempt = 0;
        loop {
            attempt += 1;
            bar.set_position(0);
            bar.set_message(if attempt > 1 { "retrying" } else { "downloading" });
            let _ = fs::remove_file(&temp).await;
            match self.download_into(package, &temp, bar).await {
                Ok(()) => break,
                Err(error) => {
                    let retry = attempt < 4 && is_transient(&error);
                    if !retry {
                        let _ = fs::remove_file(&temp).await;
                        return Err(error);
                    }
                    self.counters.retries.fetch_add(1, Ordering::Relaxed);
                    bar.println(format!("rpt: retry {attempt}/3 for {}: {error:#}", package.name));
                    tokio::time::sleep(std::time::Duration::from_millis(
                        250u64 * (1u64 << (attempt - 1)),
                    )).await;
                }
            }
        }
        bar.set_message("verifying");
        if !verify(&temp, &package.sha256).await.context(ErrorCategory::System)? {
            let _ = fs::remove_file(&temp).await;
            bail!("SHA-256 mismatch: {}", package.name);
        }
        fs::rename(&temp, &target).await.context(ErrorCategory::System)?;
        self.counters.verified.fetch_add(1, Ordering::Relaxed);
        bar.finish_with_message("verified");
        Ok(target)
    }

    async fn download_into(&self, package: &Package, temp: &Path, bar: &ProgressBar) -> Result<()> {
        let segmented = self.segments > 1 && package.size >= 1024 * 1024
            && self.accepts_ranges(&package.url, package.size).await;
        if !segmented {
            bar.set_message("single stream");
            return self.download_single(package, temp, bar).await;
        }
        let count = (self.segments as u64).min(package.size) as usize;
        bar.set_message(format!("{count} segments"));
        let file = OpenOptions::new().create(true).write(true).truncate(true).open(temp).await.context(ErrorCategory::System)?;
        file.set_len(package.size).await.context(ErrorCategory::System)?;
        drop(file);
        let mut tasks = tokio::task::JoinSet::new();
        for idx in 0..count {
            let start = (idx as u64 * package.size) / count as u64;
            let end = ((idx as u64 + 1) * package.size) / count as u64 - 1;
            let client = self.client.clone();
            let url = package.url.clone();
            let path = temp.to_path_buf();
            let bar = bar.clone();
            let package_size = package.size;
            let counters = self.counters.clone();
            tasks.spawn(async move { fetch_range(client, url, path, (start, end, package_size), bar, counters).await });
        }
        while let Some(joined) = tasks.join_next().await {
            if let Err(e) = joined.context("segment task panicked").context(ErrorCategory::System)? {
                tasks.abort_all();
                return Err(e);
            }
        }
        Ok(())
    }

    async fn accepts_ranges(&self, url: &str, expected_size: u64) -> bool {
        // Probe using a real range request: some mirrors support 206 even when
        // HEAD does not expose trustworthy Content-Length / Accept-Ranges.
        self.counters.requests.fetch_add(1, Ordering::Relaxed);
        let Ok(response) = self.client.get(url)
            .header(header::RANGE, "bytes=0-0")
            .header(header::ACCEPT_ENCODING, "identity")
            .send().await else { return false };
        if response.status() != StatusCode::PARTIAL_CONTENT { return false; }
        if response.headers().get(header::CONTENT_ENCODING).is_some() { return false; }
        let matches = response.headers().get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| valid_content_range(value, 0, 0, expected_size));
        if !matches { return false; }
        match response.bytes().await {
            Ok(bytes) if bytes.len() == 1 => {
                self.counters.bytes.fetch_add(1, Ordering::Relaxed);
                true
            }
            _ => false
        }
    }

    async fn download_single(&self, package: &Package, temp: &Path, bar: &ProgressBar) -> Result<()> {
        self.counters.requests.fetch_add(1, Ordering::Relaxed);
        let response = self.client.get(&package.url).send().await?.error_for_status()?;
        if let Some(len) = response.content_length() {
            if len != package.size { bail!("unexpected Content-Length for {}", package.name); }
        }
        let mut file = OpenOptions::new().create(true).write(true).truncate(true).open(temp).await.context(ErrorCategory::System)?;
        let mut written = 0u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            written += chunk.len() as u64;
            if written > package.size { bail!("download exceeds declared size"); }
            file.write_all(&chunk).await.context(ErrorCategory::System)?;
            self.counters.bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            bar.inc(chunk.len() as u64);
        }
        file.flush().await.context(ErrorCategory::System)?;
        if written != package.size { bail!("truncated download of {}", package.name); }
        Ok(())
    }
}

/// Confirm exact byte offsets and representation size; never trust a prefix match.
fn valid_content_range(value: &str, start: u64, end: u64, total: u64) -> bool {
    value.trim() == format!("bytes {start}-{end}/{total}")
}

async fn fetch_range(client: Client, url: String, path: PathBuf, range: (u64, u64, u64), bar: ProgressBar, counters: Arc<Counters>) -> Result<()> {
    let (start, end, total_size) = range;
    counters.requests.fetch_add(1, Ordering::Relaxed);
    counters.range_parts.fetch_add(1, Ordering::Relaxed);
    let response = client.get(&url)
        .header(header::RANGE, format!("bytes={start}-{end}"))
        .header(header::ACCEPT_ENCODING, "identity")
        .send().await?;
    if response.status() != StatusCode::PARTIAL_CONTENT {
        bail!("server did not honor byte range {start}-{end}");
    }
    if response.headers().get(header::CONTENT_ENCODING).is_some() {
        bail!("range response uses content encoding");
    }
    let content_range = response.headers()
        .get(header::CONTENT_RANGE)
        .context("missing Content-Range")?
        .to_str()?;
    if !valid_content_range(content_range, start, end, total_size) {
        bail!("incorrect Content-Range: {content_range}");
    }
    let mut file = OpenOptions::new().write(true).open(&path).await.context(ErrorCategory::System)?;
    file.seek(SeekFrom::Start(start)).await.context(ErrorCategory::System)?;
    let mut written = 0u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        written += chunk.len() as u64;
        if written > end - start + 1 { bail!("range overflow"); }
        file.write_all(&chunk).await.context(ErrorCategory::System)?;
        counters.bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        bar.inc(chunk.len() as u64);
    }
    file.flush().await.context(ErrorCategory::System)?;
    if written != end - start + 1 { bail!("range truncated"); }
    Ok(())
}

fn is_transient(error: &anyhow::Error) -> bool {
    error.chain().filter_map(|cause| cause.downcast_ref::<reqwest::Error>()).any(|e| {
        if let Some(status) = e.status() {
            return status == StatusCode::REQUEST_TIMEOUT
                || status == StatusCode::TOO_MANY_REQUESTS
                || status.is_server_error();
        }
        e.is_connect() || e.is_timeout() || e.is_request() || e.is_body()
    })
}

pub async fn verify(path: &Path, expected_hex: &str) -> Result<bool> {
    let mut file = fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let len = file.read(&mut buf).await?;
        if len == 0 { break; }
        hasher.update(&buf[..len]);
    }
    Ok(hex::encode(hasher.finalize()).eq_ignore_ascii_case(expected_hex))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_zero_concurrency() {
        assert!(Downloader::new(0, 4).is_err());
        assert!(Downloader::new(4, 0).is_err());
    }
    #[tokio::test]
    async fn verifies_known_hash() {
        let path = std::env::temp_dir().join(format!("rpt-hash-test-{}", std::process::id()));
        fs::write(&path, b"abc").await.unwrap();
        assert!(verify(&path, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad").await.unwrap());
        fs::remove_file(path).await.unwrap();
    }
    #[tokio::test]
    async fn cache_hit_counts_as_verified_without_network() {
        let root = std::env::temp_dir().join(format!("rpt-cache-stats-{}", std::process::id()));
        fs::create_dir_all(&root).await.unwrap();
        fs::write(root.join("cached.deb"), b"abc").await.unwrap();
        let package = Package {
            name: "cached".into(),
            url: "https://example.org/cached.deb".into(),
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
            size: 3,
            filename: "cached.deb".into(),
        };
        let engine = Downloader::new(1, 1).unwrap();
        engine.download_one(&package, &root).await.unwrap();
        assert_eq!(engine.counters.cache_hits.load(Ordering::Relaxed), 1);
        assert_eq!(engine.counters.verified.load(Ordering::Relaxed), 1);
        assert_eq!(engine.counters.bytes.load(Ordering::Relaxed), 0);
        fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn classifies_permanent_and_retryable_errors() {
        let missing = reqwest::Client::new()
            .get("https://example.com/")
            .build()
            .unwrap();
        // This checks that non-transport errors do not trigger retries.
        let _ = missing;
        assert!(!is_transient(&anyhow::anyhow!("SHA-256 mismatch")));
        assert!(!is_transient(&anyhow::anyhow!("unexpected Content-Length")));
    }

    #[test]
    fn validates_exact_content_ranges() {
        assert!(valid_content_range("bytes 0-0/3680654", 0, 0, 3680654));
        assert!(valid_content_range("bytes 1024-2047/3680654", 1024, 2047, 3680654));
        assert!(!valid_content_range("bytes 0-0/999", 0, 0, 3680654));
        assert!(!valid_content_range("bytes 0-1023/3680654", 0, 0, 3680654));
        assert!(!valid_content_range("bytes */3680654", 0, 0, 3680654));
    }

}
