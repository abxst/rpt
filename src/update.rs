//! Isolated, concurrent acquisition of authenticated Debian repository metadata.
//! Does not modify /var/lib/apt/lists or dpkg state.
use anyhow::{anyhow, bail, Context, Result};
use futures_util::{stream, StreamExt};
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use crate::error::{code, describe, emit, classify, ErrorCategory, ErrorCode, LogFormat};
use reqwest::{Client, Url};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Duration,
    sync::{Arc, atomic::{AtomicU64, Ordering}},
    io::IsTerminal,
};
use tokio::{fs, io::AsyncWriteExt, process::Command, sync::{Semaphore, mpsc, oneshot, Mutex}, task::JoinSet};


/// Options for the isolated metadata acquisition (not an APT drop-in).
#[derive(Debug, Clone, Copy)]
pub struct UpdateOptions {
    pub workers: usize,
    pub retries: usize,
    pub quiet: u8,
    pub progress: bool,
    pub log_format: LogFormat,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self { workers: 8, retries: 2, quiet: 0, progress: true, log_format: LogFormat::Text }
    }
}

#[derive(Default)]
struct Stats {
    attempts: AtomicU64,
    retries: AtomicU64,
}

struct IndexJob {
    client: Client,
    url: String,
    file: PathBuf,
    entry: Index,
    bar: ProgressBar,
    reply: oneshot::Sender<Result<u64>>,
}

fn update_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{spinner:.cyan} {prefix:38} [{bar:24.cyan/blue}] {bytes:>9}/{total_bytes:9} {bytes_per_sec:>10} {eta:>5} {msg}"
    ).expect("static progress style").progress_chars("=>-")
}

fn transient(error: &anyhow::Error) -> bool {
    error.chain().filter_map(|c| c.downcast_ref::<reqwest::Error>()).any(|e| {
        if let Some(status) = e.status() {
            status.as_u16() == 429 || status.as_u16() == 408 || status.is_server_error()
        } else {
            e.is_timeout() || e.is_connect() || e.is_body() || e.is_request()
        }
    })
}

/// Workers pull from a bounded FIFO queue. Network concurrency is additionally
/// limited by a global semaphore shared across all repositories.
async fn worker_loop(
    receiver: Arc<Mutex<mpsc::Receiver<IndexJob>>>,
    budget: Arc<Semaphore>,
    stats: Arc<Stats>,
    opts: UpdateOptions,
) {
    loop {
        let Some(job) = receiver.lock().await.recv().await else { break };
        let mut outcome = None;
        for attempt in 0..=opts.retries {
            job.bar.set_position(0);
            job.bar.set_message(if attempt == 0 { "downloading" } else { "retrying" });
            stats.attempts.fetch_add(1, Ordering::Relaxed);
            let _ = fs::remove_file(&job.file).await;
            let result = fetch_index(
                job.client.clone(), job.url.clone(), job.file.clone(),
                job.entry.clone(), budget.clone(), job.bar.clone(),
            ).await;
            match result {
                Ok(bytes) => {
                    job.bar.finish_with_message("verified");
                    outcome = Some(Ok(bytes));
                    break;
                }
                Err(e) if attempt < opts.retries && transient(&e) => {
                    stats.retries.fetch_add(1, Ordering::Relaxed);
                    let category = classify(&e, ErrorCategory::Download);
                    emit(opts.log_format, "retry", category, code(&e, category),
                        Some(&job.entry.path), &format!("attempt {}/{}: {}", attempt+1, opts.retries, describe(&e)));
                    tokio::time::sleep(Duration::from_millis(250u64.saturating_mul(1u64 << attempt.min(8)))).await;
                }
                Err(e) => {
                    job.bar.abandon_with_message("failed");
                    outcome = Some(Err(e.context(format!("index {}", job.entry.path))));
                    break;
                }
            }
        }
        let _ = job.reply.send(outcome.expect("at least one attempt"));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub uri: String,
    pub suite: String,
    pub components: Vec<String>,
    pub keyring: PathBuf,
}

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

fn parse_stanza(stanza: &str) -> Result<Vec<Repo>> {
    let mut fields = BTreeMap::<String, String>::new();
    for line in stanza.lines() {
        if line.trim().is_empty() || line.starts_with('#') { continue; }
        if line.starts_with(' ') {
            bail!("inline/multiline Signed-By and continuation fields are not yet supported");
        }
        let (key, value) = line.split_once(':').context("invalid Deb822 source field")?;
        if fields.insert(key.to_owned(), value.trim().to_owned()).is_some() {
            bail!("duplicate source field {key}");
        }
    }
    if fields.get("Enabled").is_some_and(|x| x.eq_ignore_ascii_case("no")) {
        return Ok(Vec::new());
    }
    let types = words(fields.get("Types").context("missing Types")?);
    if !types.iter().any(|t| t == "deb") {
        return Ok(Vec::new());
    }
    if types.iter().any(|t| t != "deb" && t != "deb-src") {
        bail!("unsupported source type");
    }
    if fields.get("Trusted").is_some_and(|x| x.eq_ignore_ascii_case("yes")) {
        bail!("Trusted: yes would bypass repository verification");
    }
    if let Some(arches) = fields.get("Architectures") {
        if !words(arches).iter().any(|arch| arch == "amd64") { return Ok(Vec::new()); }
    }
    let key = fields.get("Signed-By").context("Signed-By keyring required")?;
    if key.split_whitespace().count() != 1 || !key.starts_with('/') {
        bail!("Signed-By must be one absolute keyring path");
    }
    let keyring = PathBuf::from(key);
    let suites = words(fields.get("Suites").context("missing Suites")?);
    let components = words(fields.get("Components").context("missing Components")?);
    if suites.is_empty() || components.is_empty() { bail!("empty Suites or Components"); }
    let mut out = Vec::new();
    for uri in words(fields.get("URIs").context("missing URIs")?) {
        let url = Url::parse(&uri)?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none()
            || url.query().is_some() || url.fragment().is_some()
        { bail!("unsupported repository URI"); }
        for suite in &suites {
            if suite.contains('/') || suite.contains("..") || suite.is_empty() {
                bail!("unsupported exact-path suite {suite}");
            }
            if components.iter().any(|c| !safe_component(c)) {
                bail!("unsafe repository component");
            }
            out.push(Repo {
                uri: uri.trim_end_matches('/').to_owned(),
                suite: suite.clone(), components: components.clone(),
                keyring: keyring.clone(),
            });
        }
    }
    Ok(out)
}

fn safe_component(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && value != "." && value != ".."
}

/// Parses a restricted safe subset of APT sources. Unsupported forms fail closed.
pub fn parse_sources(root: &Path) -> Result<Vec<Repo>> {
    let sources = crate::sources::list_sources(root)?;
    let mut result = Vec::new();
    for item in sources {
        if item.starts_with("deb ") || item.starts_with("deb\t") {
            // Legacy .list entries with [signed-by=...] are accepted; other
            // option combinations need explicit implementation rather than guessing.
            let mut pieces = item.split_whitespace();
            let _deb = pieces.next();
            let first = pieces.next().context("missing repository URI")?;
            let (key, uri) = if first == "[signed-by=" {
                bail!("invalid legacy options");
            } else if first.starts_with("[signed-by=") && first.ends_with(']') {
                (first.trim_start_matches("[signed-by=").trim_end_matches(']'), pieces.next().context("missing URI")?)
            } else {
                bail!("legacy sources must specify [signed-by=/absolute/keyring.gpg]");
            };
            let suite = pieces.next().context("missing suite")?;
            let components = pieces.map(str::to_owned).collect::<Vec<_>>();
            let fields = format!("Types: deb\nURIs: {uri}\nSuites: {suite}\nComponents: {}\nSigned-By: {key}", components.join(" "));
            result.extend(parse_stanza(&fields)?);
        } else if item.starts_with("deb-src ") || item.starts_with("deb-src\t") {
            continue;
        } else if item.contains("Types:") {
            result.extend(parse_stanza(&item)?);
        } else {
            bail!("unsupported APT source format");
        }
    }
    if result.is_empty() { bail!("no supported signed binary repositories"); }
    let mut dedup = BTreeSet::new();
    result.retain(|r| dedup.insert((r.uri.clone(), r.suite.clone(), r.keyring.clone())));
    Ok(result)
}

#[derive(Debug, Clone)]
struct Index {
    path: String,
    size: u64,
    digest: String,
}

fn release_hashes(signed: &str) -> Result<BTreeMap<String, (u64, String)>> {
    if !signed.starts_with("-----BEGIN PGP SIGNED MESSAGE-----") {
        bail!("not a clearsigned InRelease");
    }
    let clear = signed.split_once("\n\n").context("invalid clearsigned headers")?.1;
    let clear = clear.split("-----BEGIN PGP SIGNATURE-----").next()
        .context("missing signature block")?;
    let mut in_sha = false;
    let mut hashes = BTreeMap::new();
    for line in clear.lines() {
        if line == "SHA256:" { in_sha = true; continue; }
        if in_sha {
            if !line.starts_with(' ') { in_sha = false; continue; }
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 3 { bail!("malformed SHA256 entry"); }
            let (digest, size, path) = (fields[0], fields[1].parse::<u64>()?, fields[2]);
            if digest.len() != 64 || hex::decode(digest).is_err()
                || path.starts_with('/') || path.split('/').any(|part| part == ".." || part.is_empty()) {
                bail!("invalid SHA256 entry");
            }
            if hashes.insert(path.to_owned(), (size, digest.to_owned())).is_some() {
                bail!("duplicate SHA256 entry");
            }
        }
    }
    if hashes.is_empty() { bail!("InRelease has no SHA256 checksums"); }
    Ok(hashes)
}

fn indexes(repo: &Repo, hashes: &BTreeMap<String, (u64, String)>) -> Result<Vec<Index>> {
    let mut result = Vec::new();
    for component in &repo.components {
        let base = format!("{component}/binary-amd64/Packages");
        let choices = [format!("{base}.xz"), format!("{base}.gz"), base];
        let selected = choices.iter().find_map(|p| hashes.get(p).map(|(size, digest)| Index {
            path: p.clone(), size: *size, digest: digest.clone(),
        }));
        let index = selected.ok_or_else(|| anyhow!("missing SHA256 for {component}/binary-amd64/Packages"))?;
        if index.size == 0 || index.size > 256 * 1024 * 1024 {
            bail!("repository index size outside supported bounds");
        }
        result.push(index);
    }
    Ok(result)
}

fn scoped_name(repo: &Repo) -> String {
    let mut hasher = Sha256::new();
    hasher.update(repo.uri.as_bytes());
    hasher.update(b"\0");
    hasher.update(repo.suite.as_bytes());
    let h = hex::encode(hasher.finalize());
    h[..24].to_owned()
}

async fn get_limited(client: &Client, url: &str, maximum: usize, budget: Arc<Semaphore>) -> Result<Vec<u8>> {
    let _permit = budget.acquire().await?;
    let response = client.get(url).send().await?.error_for_status()?;
    if response.content_length().is_some_and(|len| len > maximum as u64) {
        bail!("response too large");
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len() + chunk.len() > maximum { bail!("response too large"); }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn verify_signature(path: &Path, keyring: &Path) -> Result<()> {
    if !keyring.is_file() { bail!("Signed-By keyring not found: {}", keyring.display()); }
    let result = Command::new("gpgv")
        .arg("--keyring").arg(keyring).arg(path)
        .output().await.context("executing gpgv")?;
    if !result.status.success() { bail!("InRelease signature verification failed: {}", String::from_utf8_lossy(&result.stderr)); }
    Ok(())
}

async fn fetch_index(client: Client, url: String, path: PathBuf, expected: Index, budget: Arc<Semaphore>, bar: ProgressBar) -> Result<u64> {
    let _permit = budget.acquire().await?;
    let response = client.get(&url).send().await?.error_for_status()?;
    if response.content_length().is_some_and(|len| len != expected.size) {
        return Err(anyhow!("index Content-Length mismatch: {}", expected.path)
            .context(ErrorCode::DownloadIndex));
    }
    let mut file = fs::File::create(&path).await?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0u64;
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        downloaded += chunk.len() as u64;
        if downloaded > expected.size {
            return Err(anyhow!("index exceeds Release size").context(ErrorCode::DownloadIndex));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        bar.inc(chunk.len() as u64);
    }
    file.flush().await?;
    if downloaded != expected.size || hex::encode(hasher.finalize()) != expected.digest {
        return Err(anyhow!("index size/SHA256 mismatch: {}", expected.path)
            .context(ErrorCode::DownloadIndex));
    }
    Ok(downloaded)
}


async fn update_repo(
    client: Client, repo: Repo, dest: PathBuf, budget: Arc<Semaphore>,
    opts: UpdateOptions, multi: MultiProgress, stats: Arc<Stats>,
) -> Result<(usize, u64)> {
    let name = scoped_name(&repo);
    let stage = dest.join(format!(".{name}-{}", std::process::id()));
    fs::create_dir_all(&stage).await?;
    let work = async {
        let base = format!("{}/dists/{}", repo.uri, repo.suite);
        let signed = get_limited(&client, &format!("{base}/InRelease"), 16 * 1024 * 1024, budget.clone()).await
            .with_context(|| format!("fetching {base}/InRelease"))?;
        let release_file = stage.join("InRelease");
        fs::write(&release_file, &signed).await?;
        verify_signature(&release_file, &repo.keyring).await
            .context(ErrorCode::DownloadSignature)
            .with_context(|| format!("authenticating {base}/InRelease"))?;
        let hashes = release_hashes(std::str::from_utf8(&signed)?)
            .context(ErrorCode::DownloadIndex)?;
        let entries = indexes(&repo, &hashes)
            .context(ErrorCode::DownloadIndex)?;
        let total = entries.len();

        // Index jobs are dispatched to a bounded FIFO channel; N workers
        // pull tasks rather than creating one task per HTTP request.
        let (sender, receiver) = mpsc::channel::<IndexJob>(opts.workers.saturating_mul(2).max(1));
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = JoinSet::new();
        for _ in 0..opts.workers.min(total.max(1)) {
            workers.spawn(worker_loop(receiver.clone(), budget.clone(), stats.clone(), opts));
        }
        let mut replies = Vec::with_capacity(total);
        for (index, entry) in entries.into_iter().enumerate() {
            let suffix = if entry.path.ends_with(".xz") { "xz" }
                else if entry.path.ends_with(".gz") { "gz" } else { "raw" };
            let file = stage.join(format!("index-{index}.{suffix}"));
            let label = format!("{}:{}", repo.suite, entry.path);
            let bar = multi.add(ProgressBar::new(entry.size));
            bar.set_style(update_style());
            bar.set_prefix(label.chars().take(38).collect::<String>());
            bar.set_message("queued");
            let (reply, rx) = oneshot::channel();
            let job = IndexJob {
                client: client.clone(), url: format!("{base}/{}", entry.path),
                file, entry, bar, reply,
            };
            sender.send(job).await.context("index queue has stopped")?;
            replies.push(rx);
        }
        drop(sender);
        let mut bytes = signed.len() as u64;
        let mut failure = None;
        for reply in replies {
            match reply.await.context("index worker exited without reporting")? {
                Ok(value) => bytes += value,
                Err(error) => { if failure.is_none() { failure = Some(error); } }
            }
        }
        while let Some(joined) = workers.join_next().await {
            joined.context("index scheduler worker panicked")?;
        }
        if let Some(error) = failure { return Err(error); }

        // Preserve existing verified cache until all new index files are checked.
        let final_dir = dest.join(&name);
        let old_dir = dest.join(format!(".{name}-old-{}", std::process::id()));
        if fs::try_exists(&final_dir).await? { fs::rename(&final_dir, &old_dir).await?; }
        if let Err(error) = fs::rename(&stage, &final_dir).await {
            if fs::try_exists(&old_dir).await? { fs::rename(&old_dir, &final_dir).await?; }
            return Err(error.into());
        }
        if fs::try_exists(&old_dir).await? { fs::remove_dir_all(old_dir).await?; }
        Ok((total, bytes))
    }.await;
    if work.is_err() { let _ = fs::remove_dir_all(&stage).await; }
    work
}

/// Update independently authenticated repositories concurrently into an isolated cache.
pub async fn update(root: &Path, dest: &Path, opts: UpdateOptions) -> Result<()> {
    if !(1..=64).contains(&opts.workers) { bail!("workers must be 1..=64"); }
    if opts.retries > 10 { bail!("retries must be 0..=10"); }
    let repositories = parse_sources(root).context(ErrorCategory::System)?;
    fs::create_dir_all(dest).await?;
    let client = Client::builder().timeout(Duration::from_secs(180)).build()?;
    let count = repositories.len();
    let budget = Arc::new(Semaphore::new(opts.workers));
    let stats = Arc::new(Stats::default());
    let multi = MultiProgress::new();
    if !opts.progress || opts.log_format == LogFormat::Json || !std::io::stderr().is_terminal() {
        multi.set_draw_target(ProgressDrawTarget::hidden());
    }
    multi.set_move_cursor(true);
    let results = stream::iter(repositories.into_iter().map(|repo| {
        let client = client.clone();
        let dest = dest.to_path_buf();
        let budget = budget.clone();
        let stats = stats.clone();
        let multi = multi.clone();
        async move {
            let name = format!("{} {}", repo.uri, repo.suite);
            (name, update_repo(client, repo, dest, budget, opts, multi, stats).await)
        }
    })).buffer_unordered(opts.workers).collect::<Vec<_>>().await;
    let mut good = 0;
    let mut total = 0;
    let mut bytes = 0;
    for (name, result) in results {
        match result {
            Ok((indexes, size)) => {
                good += 1;
                total += indexes;
                bytes += size;
                if opts.quiet == 0 { println!("Verified: {name} ({indexes} indexes)"); }
            }
            Err(error) => {
                let category = classify(&error, ErrorCategory::Download);
                emit(opts.log_format, "error", category, code(&error, category),
                    Some(&name), &describe(&error));
            }
        }
    }
    if opts.quiet < 2 {
        println!("Update summary: {good}/{count} repositories, {total} indexes, {:.2} MiB transferred, {} requests, {} retries",
            bytes as f64 / 1_048_576.0,
            stats.attempts.load(Ordering::Relaxed), stats.retries.load(Ordering::Relaxed));
    }
    if good != count {
        return Err(anyhow!("{}/{} repository update(s) failed", count - good, count)
            .context(ErrorCategory::Download));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_signed_deb822() {
        let root = std::env::temp_dir().join(format!("rpt-update-sources-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sources.list.d")).unwrap();
        std::fs::write(root.join("sources.list.d/ubuntu.sources"), "Types: deb\nURIs: https://example.com/ubuntu\nSuites: noble noble-updates\nComponents: main universe\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n").unwrap();
        let r = parse_sources(&root).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].components, vec!["main", "universe"]);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn rejects_unsigned_or_untrusted() {
        assert!(parse_stanza("Types: deb\nURIs: https://example.org\nSuites: noble\nComponents: main").is_err());
        assert!(parse_stanza("Types: deb\nURIs: https://example.org\nSuites: noble\nComponents: main\nSigned-By: /tmp/key.gpg\nTrusted: yes").is_err());
    }
    #[test]
    fn checks_release_checksum_metadata() {
        let text = "-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\nOrigin: Ubuntu\nSHA256:\n abcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcdabcd 123 main/binary-amd64/Packages.xz\n-----BEGIN PGP SIGNATURE-----";
        let hashes = release_hashes(text).unwrap();
        let repo = Repo { uri: "https://example.org".into(), suite: "noble".into(), components: vec!["main".into()], keyring: "/tmp/none".into() };
        assert_eq!(indexes(&repo, &hashes).unwrap().len(), 1);
    }
}
