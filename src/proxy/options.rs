use serde::{Deserialize, Serialize};

use super::valid_hostname;

pub const MAX_PATHS: usize = 20;
pub const MAX_ERROR_PAGE: usize = 256 * 1024;
pub const MAX_TIMEOUT: u32 = 3600;
pub const MAX_BODY_MB: u32 = 10 * 1024;
const MAX_COMPRESS_TYPES: usize = 50;
/// Where error pages live in the shared static server, one folder per deployment.
pub const ERRORS_DIR: &str = "_errors";

/// What a domain does beyond sending everything to one service. Applied by managed Traefik.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteOptions {
    /// An origin like `https://example.com`. Every request goes there with its path and query kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub compress: bool,
    /// Content types to compress. Empty means Traefik's default of most types.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compress_types: Vec<String>,
    /// Smaller answers aren't compressed. Traefik's default is 1024.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compress_min_bytes: Option<u32>,
    /// Only gzip, not Brotli or Zstandard.
    #[serde(default, skip_serializing_if = "is_false")]
    pub compress_gzip_only: bool,
    /// Retry refused connections while the service restarts. Unset means on, except where an error page applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<bool>,
    /// Seconds to wait for a connection and for the answer's headers before answering 504.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
    /// Bigger requests get 413. Requests and answers are held whole on the way through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_body_mb: Option<u32>,
    /// The domain's own service gets its container address as `Host` instead of the domain. Paths keep the domain.
    #[serde(default, skip_serializing_if = "is_false")]
    pub rewrite_host: bool,
    /// Paths that go to another service, the longest match first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<PathRule>,
    /// Shown when the domain's service doesn't answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_page: Option<ErrorPage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathRule {
    pub path: String,
    /// Only this path, not everything under it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub exact: bool,
    pub service: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_page: Option<ErrorPage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorPage {
    pub body: String,
    /// Sent as JSON instead of HTML, for APIs.
    #[serde(default, skip_serializing_if = "is_false")]
    pub json: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl TryFrom<String> for RouteOptions {
    type Error = serde_json::Error;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        serde_json::from_str(&text)
    }
}

impl RouteOptions {
    pub fn is_empty(&self) -> bool {
        *self == RouteOptions::default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("route options serialize")
    }

    /// Trims what people type, so `https://example.com/` and `api/` are stored as `https://example.com` and `/api/`.
    pub fn normalized(mut self) -> RouteOptions {
        self.redirect = self.redirect.map(|r| r.trim().trim_end_matches('/').to_owned()).filter(|r| !r.is_empty());
        if self.compress {
            self.compress_types = self.compress_types.iter().map(|t| t.trim().to_ascii_lowercase()).filter(|t| !t.is_empty()).collect();
        } else {
            self.compress_types.clear();
            self.compress_min_bytes = None;
            self.compress_gzip_only = false;
        }
        for rule in &mut self.paths {
            let path = rule.path.trim();
            rule.path = if path.starts_with('/') { path.to_owned() } else { format!("/{path}") };
            rule.service = rule.service.trim().to_owned();
        }
        self
    }

    pub fn check(&self) -> Result<(), String> {
        if let Some(target) = &self.redirect {
            check_origin(target)?;
            let only_redirect = RouteOptions { redirect: Some(target.clone()), ..Default::default() };
            if *self != only_redirect {
                return Err("a redirect sends everything elsewhere, so it can't have other settings".into());
            }
        }
        if self.timeout.is_some_and(|t| t == 0 || t > MAX_TIMEOUT) {
            return Err(format!("the timeout is 1 to {MAX_TIMEOUT} seconds"));
        }
        if self.max_body_mb.is_some_and(|mb| mb == 0 || mb > MAX_BODY_MB) {
            return Err(format!("the body limit is 1 to {MAX_BODY_MB} MB"));
        }
        if self.compress_min_bytes.is_some_and(|b| b > 100 * 1024 * 1024) {
            return Err("the minimum size to compress is at most 100 MB".into());
        }
        if self.compress_types.len() > MAX_COMPRESS_TYPES {
            return Err(format!("at most {MAX_COMPRESS_TYPES} content types"));
        }
        if let Some(bad) = self.compress_types.iter().find(|t| !valid_type(t)) {
            return Err(format!("{bad} isn't a content type like text/html"));
        }
        if self.paths.len() > MAX_PATHS {
            return Err(format!("a domain can have at most {MAX_PATHS} paths"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for rule in &self.paths {
            check_path(&rule.path)?;
            if !seen.insert((rule.path.as_str(), rule.exact)) {
                return Err(format!("the path {} is listed twice", rule.path));
            }
            if rule.service.is_empty() || !rule.service.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
                return Err(format!("pick a service for {}", rule.path));
            }
            if rule.port == 0 {
                return Err(format!("pick a port for {}", rule.path));
            }
            if let Some(page) = &rule.error_page {
                check_page(page)?;
            }
        }
        if let Some(page) = &self.error_page {
            check_page(page)?;
        }
        Ok(())
    }

    /// Whether a router retries. `has_page` says whether an error page covers it.
    pub fn retries(&self, has_page: bool) -> bool {
        self.retry.unwrap_or(!has_page)
    }

    /// Every (service, port) the domain sends traffic to besides its own.
    pub fn targets(&self) -> impl Iterator<Item = (&str, u16)> {
        self.paths.iter().map(|r| (r.service.as_str(), r.port))
    }

    /// The error pages to install, as paths under the deployment's error folder.
    pub fn error_files(&self, domain_id: &str) -> Vec<(String, &str)> {
        let mut files = Vec::new();
        if let Some(page) = &self.error_page {
            files.push((page_file(domain_id, None, page), page.body.as_str()));
        }
        for (i, rule) in self.paths.iter().enumerate() {
            if let Some(page) = &rule.error_page {
                files.push((page_file(domain_id, Some(i), page), page.body.as_str()));
            }
        }
        files
    }
}

/// `<domain id>/default.html`, or `<domain id>/<rule index>.json` for a path's own page.
pub fn page_file(domain_id: &str, rule: Option<usize>, page: &ErrorPage) -> String {
    let name = rule.map(|i| i.to_string()).unwrap_or_else(|| "default".into());
    format!("{domain_id}/{name}.{}", if page.json { "json" } else { "html" })
}

/// The URL the static server answers an error page on.
pub fn page_query(deployment_id: &str, file: &str) -> String {
    format!("/{ERRORS_DIR}/{deployment_id}/{file}")
}

fn check_origin(target: &str) -> Result<(), String> {
    let rest = target
        .strip_prefix("https://")
        .or_else(|| target.strip_prefix("http://"))
        .ok_or_else(|| format!("{target} needs to start with https:// or http://"))?;
    let (host, port) = rest.split_once(':').map(|(h, p)| (h, Some(p))).unwrap_or((rest, None));
    if !valid_hostname(host) || port.is_some_and(|p| p.parse::<u16>().is_err()) {
        return Err(format!("{target} isn't an address like https://example.com, without a path"));
    }
    Ok(())
}

/// Paths end up inside a Traefik rule, so only plain URL characters are allowed.
fn check_path(path: &str) -> Result<(), String> {
    let plain = path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '~' | '%' | '@' | '+'));
    if path.len() < 2 || path.len() > 200 || !plain || path.split('/').any(|part| part == "..") {
        return Err(format!("{path} isn't a path like /api/ or /robots.txt"));
    }
    Ok(())
}

fn valid_type(text: &str) -> bool {
    let token = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '.' | '_' | '*'));
    text.split_once('/').is_some_and(|(kind, sub)| token(kind) && token(sub))
}

fn check_page(page: &ErrorPage) -> Result<(), String> {
    if page.body.trim().is_empty() {
        return Err("an error page can't be empty".into());
    }
    if page.body.len() > MAX_ERROR_PAGE {
        return Err(format!("an error page can be at most {} KB", MAX_ERROR_PAGE / 1024));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(path: &str, exact: bool) -> PathRule {
        PathRule { path: path.into(), exact, service: "backend".into(), port: 3420, error_page: None }
    }

    fn page(json: bool) -> Option<ErrorPage> {
        Some(ErrorPage { body: "down".into(), json })
    }

    #[test]
    fn normalizes_what_people_type() {
        let options = RouteOptions {
            paths: vec![PathRule { path: " api/ ".into(), service: " backend ".into(), ..rule("", false) }],
            redirect: Some(" https://example.com/ ".into()),
            ..Default::default()
        }
        .normalized();
        assert_eq!(options.paths[0].path, "/api/");
        assert_eq!(options.paths[0].service, "backend");
        assert_eq!(options.redirect.as_deref(), Some("https://example.com"));
        assert_eq!(RouteOptions { redirect: Some("  ".into()), ..Default::default() }.normalized().redirect, None);
    }

    #[test]
    fn checks_paths_redirects_and_pages() {
        let ok = RouteOptions { paths: vec![rule("/api/", false), rule("/robots.txt", true)], compress: true, error_page: page(false), ..Default::default() };
        assert!(ok.check().is_ok());
        assert!(RouteOptions { paths: vec![rule("/api/", false), rule("/api/", false)], ..Default::default() }.check().is_err());
        assert!(RouteOptions { paths: vec![rule("/api/", false), rule("/api/", true)], ..Default::default() }.check().is_ok());
        for bad in ["/", "/a`) || Host(`x", "/a b", "/../x", "/a\"b"] {
            assert!(RouteOptions { paths: vec![rule(bad, false)], ..Default::default() }.check().is_err(), "{bad}");
        }
        assert!(RouteOptions { paths: vec![PathRule { port: 0, ..rule("/api/", false) }], ..Default::default() }.check().is_err());
        assert!(RouteOptions { paths: vec![PathRule { service: "".into(), ..rule("/api/", false) }], ..Default::default() }.check().is_err());

        let redirect = |target: &str| RouteOptions { redirect: Some(target.into()), ..Default::default() };
        assert!(redirect("https://lufas.won.fyi").check().is_ok());
        assert!(redirect("http://example.com:8080").check().is_ok());
        for bad in ["lufas.won.fyi", "https://example.com/path", "ftp://example.com", "https://localhost", "https://x.com:port"] {
            assert!(redirect(bad).check().is_err(), "{bad}");
        }
        assert!(RouteOptions { compress: true, ..redirect("https://example.com") }.check().is_err());

        assert!(RouteOptions { error_page: Some(ErrorPage { body: " ".into(), json: false }), ..Default::default() }.check().is_err());
        let huge = Some(ErrorPage { body: "x".repeat(MAX_ERROR_PAGE + 1), json: false });
        assert!(RouteOptions { error_page: huge, ..Default::default() }.check().is_err());
    }

    #[test]
    fn checks_timeouts_body_limits_and_compression() {
        let ok = RouteOptions {
            compress: true,
            compress_types: vec!["text/html".into(), "image/svg+xml".into()],
            compress_min_bytes: Some(1024),
            compress_gzip_only: true,
            timeout: Some(60),
            max_body_mb: Some(1),
            retry: Some(false),
            rewrite_host: true,
            ..Default::default()
        };
        assert!(ok.check().is_ok());
        assert!(RouteOptions { timeout: Some(0), ..Default::default() }.check().is_err());
        assert!(RouteOptions { timeout: Some(MAX_TIMEOUT + 1), ..Default::default() }.check().is_err());
        assert!(RouteOptions { max_body_mb: Some(0), ..Default::default() }.check().is_err());
        assert!(RouteOptions { compress_types: vec!["html".into()], compress: true, ..Default::default() }.check().is_err());
        assert!(RouteOptions { compress_types: vec!["text/html\nx".into()], compress: true, ..Default::default() }.check().is_err());
        let redirect = RouteOptions { redirect: Some("https://example.com".into()), ..Default::default() };
        assert!(RouteOptions { timeout: Some(60), ..redirect.clone() }.check().is_err(), "a redirect has nothing else");
        assert!(RouteOptions { retry: Some(true), ..redirect }.check().is_err());

        let off = RouteOptions { compress_types: vec!["text/html".into()], compress_gzip_only: true, ..Default::default() }.normalized();
        assert!(off.is_empty(), "compression settings go away with compression off");
        let on = RouteOptions { compress: true, compress_types: vec![" Text/HTML ".into(), " ".into()], ..Default::default() }.normalized();
        assert_eq!(on.compress_types, ["text/html"]);
    }

    #[test]
    fn retries_stop_where_an_error_page_applies_unless_set() {
        assert!(RouteOptions::default().retries(false));
        assert!(!RouteOptions::default().retries(true));
        assert!(RouteOptions { retry: Some(true), ..Default::default() }.retries(true));
        assert!(!RouteOptions { retry: Some(false), ..Default::default() }.retries(false));
    }

    #[test]
    fn error_files_are_named_by_domain_and_rule() {
        let options = RouteOptions {
            paths: vec![rule("/robots.txt", true), PathRule { error_page: page(true), ..rule("/api/", false) }],
            error_page: page(false),
            ..Default::default()
        };
        assert_eq!(options.error_files("dm1"), vec![("dm1/default.html".to_owned(), "down"), ("dm1/1.json".to_owned(), "down")]);
        assert_eq!(page_query("d1", "dm1/1.json"), "/_errors/d1/dm1/1.json");
    }

    #[test]
    fn empty_options_store_as_an_empty_object() {
        assert!(RouteOptions::default().is_empty());
        assert_eq!(RouteOptions::default().to_json(), "{}");
        assert_eq!(RouteOptions::try_from("{}".to_owned()).unwrap(), RouteOptions::default());
        let options = RouteOptions { compress: true, ..Default::default() };
        assert_eq!(RouteOptions::try_from(options.to_json()).unwrap(), options);
    }
}
