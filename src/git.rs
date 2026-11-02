use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, bail};

/// `login` is the user and password sent over https.
pub async fn shallow_clone(url: &str, reference: Option<&str>, login: Option<(&str, &str)>, dest: &Path) -> anyhow::Result<()> {
    let url = authenticated_url(url, login)?;
    let reference = reference.map(str::to_owned);
    let dest: PathBuf = dest.to_owned();
    tokio::task::spawn_blocking(move || clone_blocking(&url, reference.as_deref(), &dest)).await?
}

fn clone_blocking(url: &str, reference: Option<&str>, dest: &Path) -> anyhow::Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::create_dir_all(dest)?;
    let interrupt = AtomicBool::new(false);
    let mut prepare = gix::prepare_clone(url, dest)
        .context("preparing clone")?
        .with_shallow(gix::remote::fetch::Shallow::DepthAtRemote(NonZeroU32::MIN));
    if let Some(reference) = reference {
        prepare = prepare.with_ref_name(Some(reference)).context("invalid git ref")?;
    }
    let (mut checkout, _) = prepare
        .fetch_then_checkout(gix::progress::Discard, &interrupt)
        .context("fetching repository")?;
    checkout.main_worktree(gix::progress::Discard, &interrupt).context("checking out")?;
    Ok(())
}

fn encode(part: &str) -> String {
    part.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

fn authenticated_url(url: &str, login: Option<(&str, &str)>) -> anyhow::Result<String> {
    let Some((user, password)) = login.filter(|(_, p)| !p.is_empty()) else { return Ok(url.to_owned()) };
    let Some(rest) = url.strip_prefix("https://") else { bail!("tokens only work with https git URLs") };
    Ok(format!("https://{}:{}@{rest}", encode(user), encode(password)))
}

pub fn token_login(token: &str) -> (&str, &str) {
    ("x-access-token", token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_goes_into_https_url() {
        assert_eq!(
            authenticated_url("https://github.com/a/b.git", Some(token_login("t0k"))).unwrap(),
            "https://x-access-token:t0k@github.com/a/b.git"
        );
        assert_eq!(
            authenticated_url("https://git.example.com/a/b.git", Some(("me@corp", "p@ss/word"))).unwrap(),
            "https://me%40corp:p%40ss%2Fword@git.example.com/a/b.git"
        );
        assert_eq!(authenticated_url("https://github.com/a/b.git", None).unwrap(), "https://github.com/a/b.git");
        assert!(authenticated_url("git@github.com:a/b.git", Some(token_login("t"))).is_err());
    }
}
