use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use rand::Rng;
use rand::distr::Alphanumeric;

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let hash: PasswordHash = Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| anyhow::anyhow!("hashing password: {e}"))?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else { return false };
    Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok()
}

pub fn random_password() -> String {
    rand::rng().sample_iter(&Alphanumeric).take(20).map(char::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_round_trip() {
        let hash = hash_password("correct horse").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("wrong", &hash));
    }
}
