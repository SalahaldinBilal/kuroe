//! Secrets at rest: identity secrets, secret env values and git tokens in specs, webhook secrets, saved secrets.
//! Stored as `enc:<key id>:<base64 of nonce and ciphertext>`. A value without the prefix is plain text.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::deploy::Spec;
use crate::deploy::spec::Source;

pub const PREFIX: &str = "enc:";
pub const KEY_FILE_ENV: &str = "KUROE_SECRET_KEY_FILE";
const KEY_FILE: &str = "secret.key";

#[derive(Clone)]
pub struct Key {
    bytes: [u8; 32],
    pub id: String,
}

impl Key {
    fn new(bytes: [u8; 32]) -> Key {
        let id = Sha256::digest(bytes).iter().take(4).map(|b| format!("{b:02x}")).collect();
        Key { bytes, id }
    }

    pub fn generate() -> Key {
        let mut bytes = [0u8; 32];
        SystemRandom::new().fill(&mut bytes).expect("the system random source works");
        Key::new(bytes)
    }

    pub fn from_hex(text: &str) -> anyhow::Result<Key> {
        let text = text.trim();
        if text.len() != 64 || !text.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("a secret key is 64 hex characters");
        }
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)?;
        }
        Ok(Key::new(bytes))
    }

    pub fn to_hex(&self) -> String {
        self.bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn aead(&self) -> LessSafeKey {
        LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, &self.bytes).expect("a 32 byte key fits ChaCha20-Poly1305"))
    }
}

/// Seals with the current key. Opens with the current key or, during a rotation, the previous one.
#[derive(Clone)]
pub struct Sealer {
    current: Key,
    previous: Option<Key>,
}

impl Sealer {
    pub fn new(current: Key, previous: Option<Key>) -> Sealer {
        Sealer { current, previous }
    }

    pub fn id(&self) -> &str {
        &self.current.id
    }

    pub fn knows(&self, id: &str) -> bool {
        self.current.id == id || self.previous.as_ref().is_some_and(|k| k.id == id)
    }

    /// `aad` ties the value to its place, like `identities/<id>/secret`, so a value copied elsewhere won't open.
    pub fn seal(&self, aad: &str, plain: &str) -> String {
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new().fill(&mut nonce).expect("the system random source works");
        let mut data = plain.as_bytes().to_vec();
        self.current
            .aead()
            .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(aad.as_bytes()), &mut data)
            .expect("sealing only fails for values far larger than any secret");
        let mut out = nonce.to_vec();
        out.extend(data);
        format!("{PREFIX}{}:{}", self.current.id, STANDARD.encode(out))
    }

    /// A keyed hash of `value` for finding equal values without opening them. Changes with the key.
    pub fn fingerprint(&self, value: &str) -> String {
        use hmac::{Hmac, Mac};
        let key = Sha256::new().chain_update(b"kuroe fingerprint").chain_update(self.current.bytes).finalize();
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC takes any key length");
        mac.update(value.as_bytes());
        mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn open(&self, aad: &str, stored: &str) -> anyhow::Result<String> {
        let Some(rest) = stored.strip_prefix(PREFIX) else { return Ok(stored.to_owned()) };
        let (id, body) = rest.split_once(':').context("a sealed value has no key id")?;
        let key = [Some(&self.current), self.previous.as_ref()]
            .into_iter()
            .flatten()
            .find(|k| k.id == id)
            .with_context(|| format!("{aad} is sealed with key {id}, which this server doesn't have"))?;
        let raw = STANDARD.decode(body).with_context(|| format!("{aad} is not valid base64"))?;
        if raw.len() < NONCE_LEN {
            bail!("{aad} is too short to be a sealed value");
        }
        let (nonce, data) = raw.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| anyhow::anyhow!("{aad} has a bad nonce"))?;
        let mut data = data.to_vec();
        let plain = key
            .aead()
            .open_in_place(nonce, Aad::from(aad.as_bytes()), &mut data)
            .map_err(|_| anyhow::anyhow!("{aad} doesn't open with key {id}, it was changed or belongs elsewhere"))?;
        Ok(String::from_utf8(plain.to_vec())?)
    }
}

pub fn is_sealed(value: &str) -> bool {
    value.starts_with(PREFIX)
}

fn key_id(value: &str) -> Option<&str> {
    value.strip_prefix(PREFIX)?.split_once(':').map(|(id, _)| id)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeySource {
    /// From `KUROE_SECRET_KEY_FILE`.
    File { path: PathBuf },
    /// `<data>/secret.key`, so a copy of the data folder carries the key.
    DataDir { path: PathBuf },
}

impl KeySource {
    pub fn path(&self) -> &Path {
        match self {
            KeySource::File { path } | KeySource::DataDir { path } => path,
        }
    }
}

/// `KUROE_SECRET_KEY_FILE` first, then `<data>/secret.key`, which is made on first start.
pub fn load(data_dir: &Path) -> anyhow::Result<(Key, KeySource)> {
    if let Some(path) = std::env::var_os(KEY_FILE_ENV).filter(|p| !p.is_empty()).map(PathBuf::from) {
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {KEY_FILE_ENV} at {}", path.display()))?;
        let key = Key::from_hex(&text).with_context(|| path.display().to_string())?;
        return Ok((key, KeySource::File { path }));
    }
    let path = data_dir.join(KEY_FILE);
    if path.exists() {
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        return Ok((Key::from_hex(&text).with_context(|| path.display().to_string())?, KeySource::DataDir { path }));
    }
    let key = Key::generate();
    write_key(&path, &key)?;
    tracing::info!("created the secret key at {}", path.display());
    Ok((key, KeySource::DataDir { path }))
}

/// Writes to a temporary file first and renames it, so nobody reads a half-written key.
pub fn write_key(path: &Path, key: &Key) -> anyhow::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&temp).with_context(|| format!("writing {}", temp.display()))?;
    writeln!(file, "{}", key.to_hex())?;
    file.sync_all()?;
    std::fs::rename(&temp, path).with_context(|| format!("moving the key to {}", path.display()))?;
    Ok(())
}

static SEALER: OnceLock<(Sealer, KeySource)> = OnceLock::new();

/// Loads the key once per process. Later calls keep the first one.
pub fn install(data_dir: &Path) -> anyhow::Result<()> {
    if SEALER.get().is_none() {
        let (key, source) = load(data_dir)?;
        let _ = SEALER.set((Sealer::new(key, None), source));
    }
    Ok(())
}

pub fn source() -> Option<&'static KeySource> {
    SEALER.get().map(|(_, s)| s)
}

#[cfg(not(test))]
pub fn sealer() -> &'static Sealer {
    &SEALER.get().expect("the secret key is loaded at startup").0
}

/// Unit tests share one fixed key instead of touching the data folder.
#[cfg(test)]
pub fn sealer() -> &'static Sealer {
    static TEST: OnceLock<Sealer> = OnceLock::new();
    TEST.get_or_init(|| Sealer::new(Key::new([7u8; 32]), None))
}

pub fn identity_aad(id: &str) -> String {
    format!("identities/{id}/secret")
}

pub fn webhook_aad(deployment: &str) -> String {
    format!("deployments/{deployment}/webhook_secret")
}

pub fn saved_secret_aad(id: &str) -> String {
    format!("saved_secrets/{id}/value")
}

fn env_aad(deployment: &str, key: &str) -> String {
    format!("deployments/{deployment}/env/{key}")
}

fn token_aad(deployment: &str) -> String {
    format!("deployments/{deployment}/git_token")
}

/// Seals the values named in `spec.secrets` and the git token. With `only_plain`, sealed values stay as they are.
fn seal_spec_with(sealer: &Sealer, deployment: &str, spec: &Spec, only_plain: bool) -> (Spec, usize) {
    let mut out = spec.clone();
    let mut sealed = 0;
    for key in &spec.secrets {
        if let Some(value) = out.container.env.get_mut(key)
            && !(only_plain && is_sealed(value))
        {
            *value = sealer.seal(&env_aad(deployment, key), value);
            sealed += 1;
        }
    }
    if let Source::Git { token: Some(token), .. } = &mut out.source
        && !(only_plain && is_sealed(token))
    {
        *token = sealer.seal(&token_aad(deployment), token);
        sealed += 1;
    }
    (out, sealed)
}

pub fn seal_spec(deployment: &str, spec: &Spec) -> Spec {
    seal_spec_with(sealer(), deployment, spec, false).0
}

fn open_spec_with(sealer: &Sealer, deployment: &str, spec: &Spec) -> anyhow::Result<Spec> {
    let mut out = spec.clone();
    for (key, value) in out.container.env.iter_mut() {
        if is_sealed(value) {
            *value = sealer.open(&env_aad(deployment, key), value)?;
        }
    }
    if let Source::Git { token: Some(token), .. } = &mut out.source {
        *token = sealer.open(&token_aad(deployment), token)?;
    }
    Ok(out)
}

pub fn open_spec(deployment: &str, spec: &Spec) -> anyhow::Result<Spec> {
    open_spec_with(sealer(), deployment, spec)
}

struct Stored {
    identities: Vec<(String, String)>,
    specs: Vec<(String, String)>,
    webhooks: Vec<(String, String)>,
    saved: Vec<(String, String)>,
}

async fn stored(db: &SqlitePool) -> sqlx::Result<Stored> {
    Ok(Stored {
        identities: sqlx::query_as("SELECT id, secret FROM identities").fetch_all(db).await?,
        specs: sqlx::query_as("SELECT id, spec FROM deployments").fetch_all(db).await?,
        webhooks: sqlx::query_as("SELECT id, webhook_secret FROM deployments WHERE webhook_secret IS NOT NULL").fetch_all(db).await?,
        saved: sqlx::query_as("SELECT id, value FROM saved_secrets").fetch_all(db).await?,
    })
}

fn spec_values(spec: &Spec) -> impl Iterator<Item = &str> {
    let token = match &spec.source {
        Source::Git { token: Some(t), .. } => Some(t.as_str()),
        _ => None,
    };
    spec.container.env.values().map(String::as_str).chain(token)
}

/// Every key id that some stored value is sealed with.
pub async fn key_ids_in_use(db: &SqlitePool) -> anyhow::Result<std::collections::BTreeSet<String>> {
    let s = stored(db).await?;
    let mut ids = std::collections::BTreeSet::new();
    let mut add = |v: &str| {
        if let Some(id) = key_id(v) {
            ids.insert(id.to_owned());
        }
    };
    s.identities.iter().for_each(|(_, v)| add(v));
    s.webhooks.iter().for_each(|(_, v)| add(v));
    s.saved.iter().for_each(|(_, v)| add(v));
    for (_, json) in &s.specs {
        let spec: Spec = serde_json::from_str(json)?;
        spec_values(&spec).for_each(&mut add);
    }
    Ok(ids)
}

/// Refuses to start when a value is sealed with a key this server doesn't have.
pub async fn check_keys(db: &SqlitePool, sealer: &Sealer) -> anyhow::Result<()> {
    let unknown: Vec<String> = key_ids_in_use(db).await?.into_iter().filter(|id| !sealer.knows(id)).collect();
    if !unknown.is_empty() {
        bail!(
            "saved secrets are sealed with key {}, but this server has key {}. Point {KEY_FILE_ENV} at the right key file",
            unknown.join(", "),
            sealer.id()
        );
    }
    Ok(())
}

/// Seals every plain value. Running it again changes nothing. Returns how many values it sealed.
pub async fn seal_plain(db: &SqlitePool, sealer: &Sealer) -> anyhow::Result<usize> {
    let s = stored(db).await?;
    let mut tx = db.begin().await?;
    let mut count = 0;
    for (id, secret) in s.identities.iter().filter(|(_, v)| !v.is_empty() && !is_sealed(v)) {
        sqlx::query("UPDATE identities SET secret = ? WHERE id = ?").bind(sealer.seal(&identity_aad(id), secret)).bind(id).execute(&mut *tx).await?;
        count += 1;
    }
    for (id, secret) in s.webhooks.iter().filter(|(_, v)| !is_sealed(v)) {
        sqlx::query("UPDATE deployments SET webhook_secret = ? WHERE id = ?").bind(sealer.seal(&webhook_aad(id), secret)).bind(id).execute(&mut *tx).await?;
        count += 1;
    }
    for (id, value) in s.saved.iter().filter(|(_, v)| !is_sealed(v)) {
        sqlx::query("UPDATE saved_secrets SET value = ?, fingerprint = ? WHERE id = ?")
            .bind(sealer.seal(&saved_secret_aad(id), value))
            .bind(sealer.fingerprint(value))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        count += 1;
    }
    for (id, json) in &s.specs {
        let spec: Spec = serde_json::from_str(json)?;
        let (sealed, n) = seal_spec_with(sealer, id, &spec, true);
        if n > 0 {
            sqlx::query("UPDATE deployments SET spec = ? WHERE id = ?").bind(serde_json::to_string(&sealed)?).bind(id).execute(&mut *tx).await?;
            count += n;
        }
    }
    tx.commit().await?;
    if count > 0 {
        compact(db).await?;
    }
    Ok(count)
}

/// Rewrites the database file so old copies of the replaced values don't linger in free pages or the WAL.
async fn compact(db: &SqlitePool) -> sqlx::Result<()> {
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(db).await?;
    sqlx::query("VACUUM").execute(db).await?;
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(db).await?;
    Ok(())
}

/// Opens every value with `old` and seals it again with `new`, in one transaction.
pub async fn reseal(db: &SqlitePool, old: &Sealer, new: &Sealer) -> anyhow::Result<usize> {
    let s = stored(db).await?;
    let mut tx = db.begin().await?;
    let mut count = 0;
    for (id, secret) in s.identities.iter().filter(|(_, v)| !v.is_empty()) {
        let aad = identity_aad(id);
        sqlx::query("UPDATE identities SET secret = ? WHERE id = ?").bind(new.seal(&aad, &old.open(&aad, secret)?)).bind(id).execute(&mut *tx).await?;
        count += 1;
    }
    for (id, secret) in &s.webhooks {
        let aad = webhook_aad(id);
        sqlx::query("UPDATE deployments SET webhook_secret = ? WHERE id = ?").bind(new.seal(&aad, &old.open(&aad, secret)?)).bind(id).execute(&mut *tx).await?;
        count += 1;
    }
    for (id, value) in &s.saved {
        let aad = saved_secret_aad(id);
        let plain = old.open(&aad, value)?;
        sqlx::query("UPDATE saved_secrets SET value = ?, fingerprint = ? WHERE id = ?")
            .bind(new.seal(&aad, &plain))
            .bind(new.fingerprint(&plain))
            .bind(id)
            .execute(&mut *tx)
            .await?;
        count += 1;
    }
    for (id, json) in &s.specs {
        let spec: Spec = serde_json::from_str(json)?;
        let (sealed, n) = seal_spec_with(new, id, &open_spec_with(old, id, &spec)?, false);
        sqlx::query("UPDATE deployments SET spec = ? WHERE id = ?").bind(serde_json::to_string(&sealed)?).bind(id).execute(&mut *tx).await?;
        count += n;
    }
    tx.commit().await?;
    compact(db).await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::deploy::spec::ContainerSpec;

    fn sealer_with(byte: u8) -> Sealer {
        Sealer::new(Key::new([byte; 32]), None)
    }

    #[test]
    fn seal_then_open() {
        let s = sealer_with(1);
        let sealed = s.seal("identities/a/secret", "ghp_token");
        assert!(sealed.starts_with(&format!("enc:{}:", s.id())), "{sealed}");
        assert_eq!(s.id().len(), 8);
        assert!(!sealed.contains("ghp_token"));
        assert_ne!(sealed, s.seal("identities/a/secret", "ghp_token"), "a fresh nonce every time");
        assert_eq!(s.open("identities/a/secret", &sealed).unwrap(), "ghp_token");
        assert_eq!(s.open("identities/a/secret", "plain").unwrap(), "plain", "plain values pass through");
    }

    #[test]
    fn tampering_another_place_or_another_key_fails() {
        let s = sealer_with(1);
        let sealed = s.seal("identities/a/secret", "ghp_token");
        let mut raw = STANDARD.decode(sealed.rsplit(':').next().unwrap()).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        let tampered = format!("enc:{}:{}", s.id(), STANDARD.encode(raw));
        assert!(s.open("identities/a/secret", &tampered).is_err());
        assert!(s.open("identities/b/secret", &sealed).is_err(), "pasted into another row");
        let other = sealer_with(2);
        let err = other.open("identities/a/secret", &sealed).unwrap_err().to_string();
        assert!(err.contains(s.id()), "names the missing key: {err}");
    }

    #[test]
    fn keys_from_hex() {
        let key = Key::generate();
        assert_eq!(Key::from_hex(&key.to_hex()).unwrap().id, key.id);
        assert!(Key::from_hex("abc").is_err());
        assert!(Key::from_hex(&"zz".repeat(32)).is_err());
    }

    fn spec_with_secret() -> Spec {
        let mut spec = Spec::new(Source::Git {
            url: "https://example.com/r.git".into(),
            reference: None,
            context: None,
            dockerfile: None,
            token: Some("ghp_x".into()),
            identity: None,
            compose_file: None,
        });
        spec.container = ContainerSpec {
            env: BTreeMap::from([("DB_PASSWORD".into(), "hunter2".into()), ("PORT".into(), "3000".into())]),
            ..Default::default()
        };
        spec.secrets = BTreeSet::from(["DB_PASSWORD".into()]);
        spec
    }

    #[test]
    fn spec_sealing_leaves_plain_env_readable() {
        let s = sealer_with(3);
        let (sealed, n) = seal_spec_with(&s, "d1", &spec_with_secret(), false);
        assert_eq!(n, 2);
        assert!(is_sealed(&sealed.container.env["DB_PASSWORD"]));
        assert_eq!(sealed.container.env["PORT"], "3000");
        assert!(matches!(&sealed.source, Source::Git { token: Some(t), .. } if is_sealed(t)));
        assert_eq!(open_spec_with(&s, "d1", &sealed).unwrap(), spec_with_secret());
        assert!(open_spec_with(&s, "d2", &sealed).is_err(), "bound to its deployment");
        assert_eq!(seal_spec_with(&s, "d1", &sealed, true).1, 0, "already sealed values stay");
    }

    async fn seed(db: &SqlitePool) {
        sqlx::query("INSERT INTO identities (id, kind, name, host, username, secret, meta, created_at) VALUES ('i1', 'registry', 'r', 'ghcr.io', 'u', 'regpass', '{}', 0)")
            .execute(db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO deployments (id, name, spec, status, created_at, updated_at, webhook_secret) VALUES ('d1', 'shop', ?, 'created', 0, 0, 'hook')")
            .bind(serde_json::to_string(&spec_with_secret()).unwrap())
            .execute(db)
            .await
            .unwrap();
    }

    async fn raw(db: &SqlitePool) -> (String, String, String) {
        sqlx::query_as("SELECT i.secret, d.spec, d.webhook_secret FROM identities i, deployments d").fetch_one(db).await.unwrap()
    }

    #[tokio::test]
    async fn migrate_pass_seals_once() {
        let db = crate::db::test_pool().await;
        seed(&db).await;
        let s = sealer_with(4);
        assert_eq!(seal_plain(&db, &s).await.unwrap(), 4);
        let first = raw(&db).await;
        assert!(is_sealed(&first.0) && is_sealed(&first.2));
        assert!(!first.1.contains("hunter2") && !first.1.contains("ghp_x") && first.1.contains("3000"));
        assert_eq!(seal_plain(&db, &s).await.unwrap(), 0);
        assert_eq!(raw(&db).await, first, "a second run changes nothing");
        assert_eq!(key_ids_in_use(&db).await.unwrap(), BTreeSet::from([s.id().to_owned()]));
        assert!(check_keys(&db, &s).await.is_ok());
        let err = check_keys(&db, &sealer_with(5)).await.unwrap_err().to_string();
        assert!(err.contains(s.id()), "{err}");
    }

    #[tokio::test]
    async fn rotation_reseals_and_the_old_key_stops_working() {
        let db = crate::db::test_pool().await;
        seed(&db).await;
        let old = sealer_with(6);
        seal_plain(&db, &old).await.unwrap();
        let before = raw(&db).await;
        let new_key = Key::new([8; 32]);
        let switching = Sealer::new(new_key.clone(), Some(Key::new([6; 32])));
        assert!(switching.knows(old.id()));
        let new = Sealer::new(new_key, None);
        assert_eq!(reseal(&db, &switching, &new).await.unwrap(), 4);
        let after = raw(&db).await;
        assert_eq!(new.open(&identity_aad("i1"), &after.0).unwrap(), "regpass");
        assert_eq!(new.open(&webhook_aad("d1"), &after.2).unwrap(), "hook");
        let spec: Spec = serde_json::from_str(&after.1).unwrap();
        assert_eq!(open_spec_with(&new, "d1", &spec).unwrap(), spec_with_secret());
        assert!(new.open(&identity_aad("i1"), &before.0).is_err(), "old ciphertext needs the dropped key");
        assert!(check_keys(&db, &old).await.is_err());
    }
}
