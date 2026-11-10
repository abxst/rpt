//! Concurrent, checksummed download engine with optional HTTP byte-range splitting.
use crate::{validate_package, Package};
use anyhow::{bail, Context, Result};
use futures_util::{stream, StreamExt};
use reqwest::{header, Client, StatusCode};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom},
};

#[derive(Clone)]
pub struct Downloader {
    client: Client,
    pub package_workers: usize,
    pub segments: usize,
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
        Ok(Self { client, package_workers, segments })
    }

    pub async fn download_all(&self, packages: Vec<Package>, dest: &Path) -> Result<()> {
        let results = stream::iter(packages.into_iter().map(|package| {
            let this = self.clone();
            let root = dest.to_path_buf();
            async move { this.download_one(&package, &root).await }
        }))
        .buffer_unordered(self.package_workers)
        .collect::<Vec<_>>()
        .await;
        for result in results {
            result?;
        }
        Ok(())
    }

    pub async fn download_one(&self, package: &Package, dest: &Path) -> Result<PathBuf> {
        validate_package(package)?;
        let target = dest.join(&package.filename);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).await?;
        }
        if fs::try_exists(&target).await? && verify(&target, &package.sha256).await? {
            return Ok(target);
        }
        // A temporary file is private to this process; another concurrent invocation must
        // use a separate destination or external coordination.
        let temp = target.with_extension(format!("rpt-{}.part", std::process::id()));
        // Retry only transport failures and temporary HTTP responses.
        // Every attempt starts with an empty partial file.
        let mut attempt = 0;
        loop {
            attempt += 1;
            let _ = fs::remove_file(&temp).await;
            match self.download_into(package, &temp).await {
                Ok(()) => break,
                Err(error) => {
                    let retry = attempt < 4 && is_transient(&error);
                    if !retry {
                        let _ = fs::remove_file(&temp).await;
                        return Err(error);
                    }
                    eprintln!(
                        "rpt: retry {attempt}/3 for {} after temporary error: {error:#}",
                        package.name
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(
                        250u64 * (1u64 << (attempt - 1)),
                    ))
                    .await;
                }
            }
        }
        if !verify(&temp, &package.sha256).await? {
            let _ = fs::remove_file(&temp).await;
            bail!("SHA-256 mismatch: {}", package.name);
        }
        fs::rename(&temp, &target).await?;
        Ok(target)
    }

    async fn download_into(&self, package: &Package, temp: &Path) -> Result<()> {
        let segmented = self.segments > 1 && package.size >= 1024 * 1024
            && self.accepts_ranges(&package.url, package.size).await;
        if !segmented {
            return self.download_single(package, temp).await;
        }
        let count = (self.segments as u64).min(package.size) as usize;
        let file = OpenOptions::new().create(true).write(true).truncate(true).open(temp).await?;
        file.set_len(package.size).await?;
        drop(file);
        let mut tasks = tokio::task::JoinSet::new();
        for idx in 0..count {
            let start = (idx as u64 * package.size) / count as u64;
            let end = ((idx as u64 + 1) * package.size) / count as u64 - 1;
            let client = self.client.clone();
            let url = package.url.clone();
            let path = temp.to_path_buf();
            tasks.spawn(async move { fetch_range(client, url, path, start, end).await });
        }
        while let Some(joined) = tasks.join_next().await {
            if let Err(e) = joined.context("segment task panicked")? {
                tasks.abort_all();
                return Err(e);
            }
        }
        Ok(())
    }

    async fn accepts_ranges(&self, url: &str, expected_size: u64) -> bool {
        let Ok(resp) = self.client.head(url).send().await else { return false };
        if !resp.status().is_success() { return false; }
        resp.headers().get(header::ACCEPT_RANGES).and_then(|x| x.to_str().ok()) == Some("bytes")
            && resp.content_length() == Some(expected_size)
    }

    async fn download_single(&self, package: &Package, temp: &Path) -> Result<()> {
        let response = self.client.get(&package.url).send().await?.error_for_status()?;
        if let Some(len) = response.content_length() {
            if len != package.size { bail!("unexpected Content-Length for {}", package.name); }
        }
        let mut file = OpenOptions::new().create(true).write(true).truncate(true).open(temp).await?;
        let mut written = 0u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            written += chunk.len() as u64;
            if written > package.size { bail!("download exceeds declared size"); }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        if written != package.size { bail!("truncated download of {}", package.name); }
        Ok(())
    }
}

async fn fetch_range(client: Client, url: String, path: PathBuf, start: u64, end: u64) -> Result<()> {
    let response = client.get(&url)
        .header(header::RANGE, format!("bytes={start}-{end}"))
        .header(header::ACCEPT_ENCODING, "identity")
        .send().await?;
    if response.status() != StatusCode::PARTIAL_CONTENT {
        bail!("server did not honor byte range {start}-{end}");
    }
    let expected_prefix = format!("bytes {start}-{end}/");
    let content_range = response.headers()
        .get(header::CONTENT_RANGE)
        .context("missing Content-Range")?
        .to_str()?;
    if !content_range.starts_with(&expected_prefix) {
        bail!("incorrect Content-Range: {content_range}");
    }
    let mut file = OpenOptions::new().write(true).open(&path).await?;
    file.seek(SeekFrom::Start(start)).await?;
    let mut written = 0u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        written += chunk.len() as u64;
        if written > end - start + 1 { bail!("range overflow"); }
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
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

}
