use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, bail};

/// `login` is the user and password sent over https. Returns the commit that was checked out.
pub async fn shallow_clone(url: &str, reference: Option<&str>, login: Option<(&str, &str)>, dest: &Path) -> anyhow::Result<String> {
    let with_login = authenticated_url(url, login)?;
    let reference = reference.map(str::to_owned);
    let dest: PathBuf = dest.to_owned();
    let cloning = with_login.clone();
    let cloned = tokio::task::spawn_blocking(move || clone_blocking(&cloning, reference.as_deref(), &dest)).await?;
    cloned.map_err(|err| anyhow::anyhow!(without_secrets(&format!("{err:#}"), url, login)))
}

/// Git errors can quote the URL they fetched, which carries the password.
fn without_secrets(message: &str, url: &str, login: Option<(&str, &str)>) -> String {
    let Some((_, password)) = login.filter(|(_, p)| !p.is_empty()) else { return message.to_owned() };
    let mut message = match authenticated_url(url, login) {
        Ok(with_login) => message.replace(&with_login, url),
        Err(_) => message.to_owned(),
    };
    for secret in [encode(password), password.to_owned()] {
        message = message.replace(&secret, "********");
    }
    message
}

fn clone_blocking(url: &str, reference: Option<&str>, dest: &Path) -> anyhow::Result<String> {
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
    let (repo, _) = checkout.main_worktree(gix::progress::Discard, &interrupt).context("checking out")?;
    Ok(repo.head_id().context("reading the checked out commit")?.to_string())
}

pub fn encode(part: &str) -> String {
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

    #[test]
    fn clone_errors_lose_the_password() {
        let url = "https://git.example.com/a/b.git";
        let login = Some(("me", "p@ss/word"));
        let message = "fetching repository: https://me:p%40ss%2Fword@git.example.com/a/b.git: 401, tried p@ss/word";
        assert_eq!(
            without_secrets(message, url, login),
            "fetching repository: https://git.example.com/a/b.git: 401, tried ********"
        );
        assert_eq!(without_secrets("not found", url, None), "not found");
    }
}
