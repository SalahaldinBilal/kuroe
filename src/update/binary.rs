use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes the download next to the current binary only when its hash matches.
pub fn stage(bytes: &[u8], expected: &str, dir: &Path) -> anyhow::Result<PathBuf> {
    let expected = expected.trim().to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("{expected:?} is not a sha256 hex digest");
    }
    let actual = sha256_hex(bytes);
    if actual != expected {
        bail!("sha256 mismatch: expected {expected}, the download is {actual}");
    }
    let path = dir.join("kuroe.new");
    std::fs::write(&path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(path)
}

pub async fn download(http: &reqwest::Client, url: &str) -> anyhow::Result<Vec<u8>> {
    let res = http.get(url).send().await?.error_for_status().with_context(|| format!("downloading {url}"))?;
    Ok(res.bytes().await?.to_vec())
}

pub async fn expected_sha(http: &reqwest::Client, url: &str, given: Option<&str>) -> anyhow::Result<String> {
    if let Some(given) = given.filter(|g| !g.trim().is_empty()) {
        return Ok(given.trim().to_owned());
    }
    let text = String::from_utf8(download(http, &format!("{url}.sha256")).await?)?;
    text.split_whitespace().next().map(str::to_owned).context("the .sha256 file is empty")
}

async fn check(new: &Path, data_dir: &Path) -> anyhow::Result<()> {
    let version = tokio::process::Command::new(new).arg("--version").output().await.context("running the new binary")?;
    let printed = String::from_utf8_lossy(&version.stdout);
    if !version.status.success() || !printed.starts_with("kuroe ") {
        bail!("the new binary does not look like kuroe: {}", printed.trim());
    }
    let migrate = tokio::process::Command::new(new).arg("--data-dir").arg(data_dir).arg("migrate").output().await?;
    if !migrate.status.success() {
        bail!("migrations failed: {}", String::from_utf8_lossy(&migrate.stderr).trim());
    }
    Ok(())
}

/// Downloads, verifies and test-runs the new binary. Any failure leaves the running version alone.
pub async fn prepare(http: &reqwest::Client, url: &str, sha: Option<&str>, data_dir: &Path) -> anyhow::Result<(PathBuf, String)> {
    let current = std::env::current_exe()?;
    let dir = current.parent().context("binary has no parent directory")?.to_owned();
    let expected = expected_sha(http, url, sha).await?;
    let bytes = download(http, url).await?;
    let staged = stage(&bytes, &expected, &dir)?;
    if let Err(err) = check(&staged, data_dir).await {
        std::fs::remove_file(&staged).ok();
        return Err(err);
    }
    Ok((staged, expected))
}

/// Keeps the current binary as kuroe.prev, moves the staged one into place and replaces this process with it.
pub fn swap_and_exec(staged: &Path) -> anyhow::Result<()> {
    let current = std::env::current_exe()?;
    let previous = current.with_file_name("kuroe.prev");
    std::fs::copy(&current, &previous).context("keeping a copy of the current binary")?;
    std::fs::rename(staged, &current).context("replacing the binary")?;
    exec(&current)
}

#[cfg(unix)]
fn exec(binary: &Path) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let err = std::process::Command::new(binary).args(args).exec();
    Err(anyhow::anyhow!("exec into the new binary failed: {err}"))
}

#[cfg(not(unix))]
fn exec(_: &Path) -> anyhow::Result<()> {
    bail!("binary self-update only works on Linux")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kuroe-stage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sha_mismatch_aborts_and_writes_nothing() {
        let dir = dir();
        let wrong = sha256_hex(b"something else");
        let err = stage(b"new binary", &wrong, &dir).unwrap_err().to_string();
        assert!(err.starts_with("sha256 mismatch"), "{err}");
        assert!(!dir.join("kuroe.new").exists());
        assert!(stage(b"new binary", "not-a-hash", &dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn matching_sha_stages_the_file() {
        let dir = dir();
        let sha = sha256_hex(b"new binary").to_ascii_uppercase();
        let path = stage(b"new binary", &format!(" {sha}\n"), &dir).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new binary");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
