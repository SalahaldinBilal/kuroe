use std::io::Write;
use std::path::{Path, PathBuf};

use rand::RngCore;

const FILE: &str = "setup-code";
const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const LENGTH: usize = 12;

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

fn generate() -> String {
    let mut bytes = [0u8; LENGTH];
    rand::rng().fill_bytes(&mut bytes);
    let chars: Vec<char> = bytes.iter().map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char).collect();
    chars.chunks(4).map(|group| group.iter().collect::<String>()).collect::<Vec<_>>().join("-")
}

fn normalize(code: &str) -> String {
    code.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

/// Reads the code, or creates it. The server and `kuroe admin setup-code` can race, so only one write wins.
pub fn get_or_create(data_dir: &Path) -> std::io::Result<String> {
    let path = path(data_dir);
    if let Ok(existing) = std::fs::read_to_string(&path)
        && !existing.trim().is_empty()
    {
        return Ok(existing.trim().to_owned());
    }
    std::fs::create_dir_all(data_dir)?;
    let code = generate();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            file.write_all(code.as_bytes())?;
            Ok(code)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(std::fs::read_to_string(&path)?.trim().to_owned()),
        Err(err) => Err(err),
    }
}

pub fn matches(data_dir: &Path, given: &str) -> bool {
    let Ok(expected) = std::fs::read_to_string(path(data_dir)) else { return false };
    let (expected, given) = (normalize(&expected), normalize(given));
    !expected.is_empty()
        && expected.len() == given.len()
        && expected.bytes().zip(given.bytes()).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}

pub fn clear(data_dir: &Path) {
    let _ = std::fs::remove_file(path(data_dir));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kuroe-setup-code-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn code_is_kept_until_cleared_and_matches_loosely() {
        let dir = dir();
        let code = get_or_create(&dir).unwrap();
        assert_eq!(code.len(), 14);
        assert!(code.split('-').all(|group| group.len() == 4));
        assert_eq!(get_or_create(&dir).unwrap(), code, "a restart keeps the same code");

        assert!(matches(&dir, &code));
        assert!(matches(&dir, &format!(" {} ", code.to_ascii_lowercase().replace('-', ""))));
        assert!(!matches(&dir, "AAAA-AAAA-AAAA"));
        assert!(!matches(&dir, ""));

        clear(&dir);
        assert!(!matches(&dir, &code), "no code once setup is done");
        assert_ne!(get_or_create(&dir).unwrap(), code);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
