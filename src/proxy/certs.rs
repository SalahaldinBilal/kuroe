use std::collections::BTreeSet;

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, ECDSA_P384_SHA384_ASN1_SIGNING, EcdsaKeyPair, KeyPair, RsaKeyPair};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::identity::{Identity, Kind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CertMode {
    /// Let's Encrypt over port 80.
    #[default]
    Http,
    /// Let's Encrypt over a Cloudflare DNS record.
    Dns,
    /// No Let's Encrypt. Domains only get saved certificates.
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Parsed {
    pub sans: Vec<String>,
    pub not_after: i64,
    pub issuer: String,
    pub cloudflare_origin: bool,
}

fn pem_blocks(text: &str, label: &str) -> Vec<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(&begin) {
        let body = &rest[start + begin.len()..];
        let Some(stop) = body.find(&end) else { break };
        let b64: String = body[..stop].chars().filter(|c| !c.is_whitespace()).collect();
        if let Ok(der) = STANDARD.decode(b64) {
            blocks.push(der);
        }
        rest = &body[stop + end.len()..];
    }
    blocks
}

pub fn is_certificate(text: &str) -> bool {
    let blocks = pem_blocks(text, "CERTIFICATE");
    !blocks.is_empty() && blocks.iter().all(|der| x509_parser::parse_x509_certificate(der).is_ok())
}

/// Reads the first certificate of a PEM chain: its names, expiry and issuer.
pub fn parse_chain(chain: &str) -> anyhow::Result<Parsed> {
    let blocks = pem_blocks(chain, "CERTIFICATE");
    let Some(leaf) = blocks.first() else { bail!("no -----BEGIN CERTIFICATE----- block found") };
    for (i, der) in blocks.iter().enumerate() {
        x509_parser::parse_x509_certificate(der).map_err(|e| anyhow::anyhow!("certificate {} in the chain can't be read: {e}", i + 1))?;
    }
    let (_, cert) = x509_parser::parse_x509_certificate(leaf).map_err(|e| anyhow::anyhow!("the certificate can't be read: {e}"))?;
    let mut sans = Vec::new();
    if let Ok(Some(ext)) = cert.subject_alternative_name() {
        for name in &ext.value.general_names {
            if let x509_parser::extensions::GeneralName::DNSName(dns) = name {
                sans.push(dns.to_ascii_lowercase());
            }
        }
    }
    if sans.is_empty()
        && let Some(cn) = cert.subject().iter_common_name().next().and_then(|cn| cn.as_str().ok())
    {
        sans.push(cn.to_ascii_lowercase());
    }
    if sans.is_empty() {
        bail!("the certificate names no hostnames");
    }
    let issuer = cert.issuer().to_string();
    Ok(Parsed {
        sans,
        not_after: cert.validity().not_after.timestamp(),
        cloudflare_origin: issuer.contains("CloudFlare Origin"),
        issuer,
    })
}

fn private_key_public(key: &str) -> anyhow::Result<Vec<u8>> {
    if let Some(der) = pem_blocks(key, "PRIVATE KEY").first() {
        if let Ok(rsa) = RsaKeyPair::from_pkcs8(der) {
            return Ok(rsa.public().as_ref().to_vec());
        }
        let rng = SystemRandom::new();
        for alg in [&ECDSA_P256_SHA256_ASN1_SIGNING, &ECDSA_P384_SHA384_ASN1_SIGNING] {
            if let Ok(ec) = EcdsaKeyPair::from_pkcs8(alg, der, &rng) {
                return Ok(ec.public_key().as_ref().to_vec());
            }
        }
        bail!("the private key is not an RSA, P-256 or P-384 key");
    }
    if let Some(der) = pem_blocks(key, "RSA PRIVATE KEY").first() {
        let rsa = RsaKeyPair::from_der(der).map_err(|e| anyhow::anyhow!("the RSA key can't be read: {e}"))?;
        return Ok(rsa.public().as_ref().to_vec());
    }
    if key.contains("BEGIN EC PRIVATE KEY") {
        bail!("convert the EC key to PKCS#8 first: openssl pkcs8 -topk8 -nocrypt -in key.pem");
    }
    bail!("no -----BEGIN PRIVATE KEY----- block found")
}

/// The key has to belong to the first certificate, or Traefik silently serves its default one.
pub fn check_key(chain: &str, key: &str) -> anyhow::Result<()> {
    let leaf = pem_blocks(chain, "CERTIFICATE").into_iter().next().context("no certificate")?;
    let (_, cert) = x509_parser::parse_x509_certificate(&leaf).map_err(|e| anyhow::anyhow!("the certificate can't be read: {e}"))?;
    let in_cert = cert.public_key().subject_public_key.data.to_vec();
    if private_key_public(key)? != in_cert {
        bail!("the private key doesn't belong to this certificate");
    }
    Ok(())
}

/// `*.example.com` covers exactly one label below it, never the apex or two levels down.
pub fn covers(san: &str, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    match san.strip_prefix("*.") {
        Some(base) => host.strip_suffix(base).and_then(|rest| rest.strip_suffix('.')).is_some_and(|label| !label.is_empty() && !label.contains('.')),
        None => san == host,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub id: String,
    pub name: String,
    pub sans: Vec<String>,
    pub not_after: i64,
    pub aop: bool,
    pub chain: String,
    pub key: String,
}

pub fn saved(identity: &Identity) -> Option<Saved> {
    if identity.kind != Kind::Certificate {
        return None;
    }
    let sans = identity.meta["sans"].as_array()?.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect();
    Some(Saved {
        id: identity.id.clone(),
        name: identity.name.clone(),
        sans,
        not_after: identity.meta["not_after"].as_i64().unwrap_or(0),
        aop: identity.meta["aop"].as_bool().unwrap_or(false),
        chain: identity.meta["chain"].as_str().unwrap_or_default().to_owned(),
        key: identity.secret.clone(),
    })
}

/// An exact name beats a wildcard, then the certificate that runs longest wins.
pub fn pick<'a>(certs: &'a [Saved], host: &str) -> Option<&'a Saved> {
    let rank = |c: &Saved| {
        let exact = c.sans.iter().any(|s| !s.starts_with("*.") && covers(s, host));
        (exact, c.not_after)
    };
    certs.iter().filter(|c| c.sans.iter().any(|s| covers(s, host))).max_by_key(|c| rank(c))
}

/// How a TLS router gets its certificate.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteCert {
    /// Let's Encrypt, optionally as one wildcard certificate for the zone.
    Acme { wildcard_zone: Option<String> },
    /// A saved certificate from Traefik's store.
    Saved { id: String, name: String, aop: bool },
    /// No resolver and no saved certificate. Traefik answers with its default certificate.
    Default,
}

pub struct Plan<'a> {
    pub mode: CertMode,
    pub wildcard_zones: &'a [String],
    pub saved: &'a [Saved],
}

pub const PIN_ACME: &str = "acme";

/// The zone a hostname sits directly below, for a `*.zone` wildcard request.
fn wildcard_zone(zones: &[String], host: &str) -> Option<String> {
    zones.iter().find(|z| covers(&format!("*.{z}"), host) || host == z.as_str()).cloned()
}

impl Plan<'_> {
    pub fn route_cert(&self, host: &str, pin: Option<&str>) -> RouteCert {
        let acme = || match self.mode {
            CertMode::None => RouteCert::Default,
            CertMode::Http => RouteCert::Acme { wildcard_zone: None },
            CertMode::Dns => RouteCert::Acme { wildcard_zone: wildcard_zone(self.wildcard_zones, host) },
        };
        let as_saved = |c: &Saved| RouteCert::Saved { id: c.id.clone(), name: c.name.clone(), aop: c.aop };
        match pin {
            Some(PIN_ACME) => acme(),
            Some(id) => self.saved.iter().find(|c| c.id == id).map(as_saved).unwrap_or_else(acme),
            None => pick(self.saved, host).map(as_saved).unwrap_or_else(acme),
        }
    }
}

pub const CERTS_DIR: &str = "/etc/traefik/certs";
pub const AOP_OPTIONS: &str = "kuroe-aop@file";
pub const AOP_CA_FILE: &str = "cloudflare-origin-pull-ca.pem";
pub const AOP_CA_URL: &str = "https://developers.cloudflare.com/ssl/static/authenticated_origin_pull_ca.pem";

/// The dynamic config listing the saved certificates in use, and the origin pull check when one asks for it.
pub fn store_config(in_use: &[&Saved]) -> Value {
    let certificates: Vec<Value> = in_use
        .iter()
        .map(|c| json!({ "certFile": format!("{CERTS_DIR}/{}.crt", c.id), "keyFile": format!("{CERTS_DIR}/{}.key", c.id) }))
        .collect();
    let mut tls = json!({ "certificates": certificates });
    if in_use.iter().any(|c| c.aop) {
        tls["options"] = json!({
            "kuroe-aop": {
                "clientAuth": { "caFiles": [format!("{CERTS_DIR}/{AOP_CA_FILE}")], "clientAuthType": "RequireAndVerifyClientCert" }
            }
        });
    }
    json!({ "tls": tls })
}

pub fn ids_in_use(certs: &[Saved], routes: &[RouteCert]) -> BTreeSet<String> {
    let used: BTreeSet<&str> = routes.iter().filter_map(|r| if let RouteCert::Saved { id, .. } = r { Some(id.as_str()) } else { None }).collect();
    certs.iter().filter(|c| used.contains(c.id.as_str())).map(|c| c.id.clone()).collect()
}

/// A fresh P-256 key and a CSR for the hostnames. The key never leaves this server.
pub fn key_and_csr(hostnames: &[String]) -> anyhow::Result<(String, String)> {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    let params = rcgen::CertificateParams::new(hostnames.to_vec())?;
    let csr = params.serialize_request(&key)?;
    Ok((key.serialize_pem(), csr.pem()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn self_signed(names: &[&str]) -> (String, String) {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let params = rcgen::CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>()).unwrap();
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    fn saved(id: &str, sans: &[&str], not_after: i64) -> Saved {
        Saved {
            id: id.into(),
            name: id.into(),
            sans: sans.iter().map(|s| s.to_string()).collect(),
            not_after,
            aop: false,
            chain: String::new(),
            key: String::new(),
        }
    }

    #[test]
    fn wildcard_covers_one_level_only() {
        assert!(covers("*.example.com", "app.example.com"));
        assert!(covers("*.example.com", "APP.example.com"));
        assert!(!covers("*.example.com", "example.com"));
        assert!(!covers("*.example.com", "a.b.example.com"));
        assert!(!covers("*.example.com", "badexample.com"));
        assert!(covers("example.com", "example.com"));
        assert!(!covers("example.com", "app.example.com"));
    }

    #[test]
    fn exact_beats_wildcard_then_longest_wins() {
        let certs = [saved("wild", &["example.com", "*.example.com"], 100), saved("shop", &["shop.example.com"], 50), saved("wild2", &["*.example.com"], 200)];
        assert_eq!(pick(&certs, "shop.example.com").unwrap().id, "shop");
        assert_eq!(pick(&certs, "api.example.com").unwrap().id, "wild2");
        assert_eq!(pick(&certs, "example.com").unwrap().id, "wild");
        assert!(pick(&certs, "other.dev").is_none());
    }

    #[test]
    fn domains_mix_saved_certificates_and_lets_encrypt() {
        let certs = [saved("origin", &["example.com", "*.example.com"], 100)];
        let zones = ["other.dev".to_owned()];
        let plan = Plan { mode: CertMode::Dns, wildcard_zones: &zones, saved: &certs };
        assert_eq!(plan.route_cert("app.example.com", None), RouteCert::Saved { id: "origin".into(), name: "origin".into(), aop: false });
        assert_eq!(plan.route_cert("app.example.com", Some(PIN_ACME)), RouteCert::Acme { wildcard_zone: None });
        assert_eq!(plan.route_cert("api.other.dev", None), RouteCert::Acme { wildcard_zone: Some("other.dev".into()) });
        assert_eq!(plan.route_cert("deep.api.other.dev", None), RouteCert::Acme { wildcard_zone: None });
        assert_eq!(plan.route_cert("x.example.com", Some("gone")), RouteCert::Acme { wildcard_zone: None }, "a removed pin falls back");
        let none = Plan { mode: CertMode::None, wildcard_zones: &[], saved: &certs };
        assert_eq!(none.route_cert("site.test", None), RouteCert::Default);
    }

    #[test]
    fn store_lists_used_certificates_and_origin_pulls_only_when_asked() {
        let mut a = saved("a", &["a.test"], 1);
        let b = saved("b", &["b.test"], 1);
        let config = store_config(&[&a, &b]);
        assert_eq!(config["tls"]["certificates"][0], json!({ "certFile": "/etc/traefik/certs/a.crt", "keyFile": "/etc/traefik/certs/a.key" }));
        assert!(config["tls"].get("options").is_none());
        a.aop = true;
        let config = store_config(&[&a]);
        assert_eq!(config["tls"]["options"]["kuroe-aop"]["clientAuth"]["clientAuthType"], "RequireAndVerifyClientCert");
        let routes = [RouteCert::Saved { id: "a".into(), name: "a".into(), aop: true }, RouteCert::Default];
        assert_eq!(ids_in_use(&[a, b], &routes), BTreeSet::from(["a".to_owned()]));
    }

    #[test]
    fn reads_names_and_checks_the_key() {
        let (chain, key) = self_signed(&["example.com", "*.example.com"]);
        let parsed = parse_chain(&chain).unwrap();
        assert_eq!(parsed.sans, ["example.com", "*.example.com"]);
        assert!(parsed.not_after > 0 && !parsed.cloudflare_origin);
        check_key(&chain, &key).unwrap();

        let (_, other_key) = self_signed(&["x.test"]);
        assert_eq!(check_key(&chain, &other_key).unwrap_err().to_string(), "the private key doesn't belong to this certificate");
        assert!(parse_chain("not a cert").is_err());
        let ec = "-----BEGIN EC PRIVATE KEY-----\nAA==\n-----END EC PRIVATE KEY-----";
        assert!(check_key(&chain, ec).unwrap_err().to_string().contains("openssl pkcs8"));
    }

    #[test]
    fn csr_comes_with_a_matching_key() {
        let (key, csr) = key_and_csr(&["example.com".into(), "*.example.com".into()]).unwrap();
        assert!(csr.starts_with("-----BEGIN CERTIFICATE REQUEST-----"));
        assert!(key.contains("BEGIN PRIVATE KEY"));
    }
}
