use serde_json::{Map, Value, json};

use super::certs::{AOP_OPTIONS, RouteCert};

pub const RETRY_MIDDLEWARE: &str = "kuroe-retry@file";
pub const HTTPS_MIDDLEWARE: &str = "kuroe-https@file";
pub const COMPRESS_MIDDLEWARE: &str = "kuroe-compress@file";
/// Traefik's built-in service for routers that only redirect.
pub const NOOP_SERVICE: &str = "noop@internal";
pub const CERT_RESOLVER: &str = "le";
/// Path routers outrank a hostname's own router, which Traefik ranks by rule length.
const PATH_PRIORITY: usize = 10_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub hostname: String,
    pub tls: bool,
    pub service: String,
    pub port: u16,
    pub upstreams: Vec<String>,
    pub cert: RouteCert,
    pub extra: Extra,
}

/// A domain's routing options, with upstreams and error page URLs filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct Extra {
    pub redirect: Option<String>,
    pub compress: Option<Compression>,
    /// Error page URL on the static server, for the domain's own service.
    pub error_page: Option<String>,
    /// Whether the domain's own router retries.
    pub retry: bool,
    pub max_body_bytes: Option<u64>,
    /// Seconds to connect and to get the answer's headers, for every service of the domain.
    pub timeout: Option<u32>,
    /// The domain's own service gets its address as `Host`. Its upstreams are then IP addresses.
    pub rewrite_host: bool,
    pub paths: Vec<PathRoute>,
}

impl Default for Extra {
    fn default() -> Self {
        Extra {
            redirect: None,
            compress: None,
            error_page: None,
            retry: true,
            max_body_bytes: None,
            timeout: None,
            rewrite_host: false,
            paths: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Compression {
    pub types: Vec<String>,
    pub min_bytes: Option<u32>,
    pub gzip_only: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathRoute {
    pub path: String,
    pub exact: bool,
    pub service: String,
    pub port: u16,
    pub upstreams: Vec<String>,
    pub error_page: Option<String>,
    pub retry: bool,
}

pub(crate) fn tls_block(cert: &RouteCert) -> Value {
    match cert {
        RouteCert::Acme { wildcard_zone: None } => json!({ "certResolver": CERT_RESOLVER }),
        RouteCert::Acme { wildcard_zone: Some(zone) } => {
            json!({ "certResolver": CERT_RESOLVER, "domains": [{ "main": zone, "sans": [format!("*.{zone}")] }] })
        }
        RouteCert::Saved { aop: true, .. } => json!({ "options": AOP_OPTIONS }),
        RouteCert::Saved { .. } | RouteCert::Default => json!({}),
    }
}

pub fn service_name(deployment: &str, service: &str, port: u16) -> String {
    format!("{deployment}-{service}-{port}")
}

/// The Traefik service for a target. Two domains can share a service but differ in timeout or Host header,
/// so those are part of the name.
pub fn service_key(deployment: &str, service: &str, port: u16, rewrite_host: bool, timeout: Option<u32>) -> String {
    let mut name = service_name(deployment, service, port);
    if rewrite_host {
        name.push_str("-ip");
    }
    if let Some(seconds) = timeout {
        name.push_str(&format!("-t{seconds}"));
    }
    name
}

pub(crate) fn router_name(deployment: &str, hostname: &str) -> String {
    format!("{deployment}-{}", hostname.replace('.', "-"))
}

pub fn shared_config() -> Value {
    json!({
        "http": {
            "middlewares": {
                "kuroe-retry": { "retry": { "attempts": 30, "initialInterval": "100ms" } },
                "kuroe-https": { "redirectScheme": { "scheme": "https", "permanent": true } },
                "kuroe-compress": { "compress": {} }
            },
            "services": { (crate::sites::SERVICE): crate::sites::service_config() }
        }
    })
}

/// A redirect to `target` that keeps the path and query.
pub fn redirect_middleware(target: &str) -> Value {
    json!({ "redirectRegex": { "regex": "^https?://[^/]+(.*)$", "replacement": format!("{target}${{1}}"), "permanent": true } })
}

/// Routers for a hostname that only redirects: HTTP goes straight to the target too, not to HTTPS first.
pub(crate) fn redirect_routers(routers: &mut Map<String, Value>, middlewares: &mut Map<String, Value>, router: &str, hostname: &str, tls: Option<&RouteCert>, target: &str) {
    let name = format!("{router}-redirect");
    middlewares.insert(name.clone(), redirect_middleware(target));
    let rule = format!("Host(`{hostname}`)");
    if let Some(cert) = tls {
        routers.insert(
            router.to_owned(),
            json!({ "rule": rule, "entryPoints": ["websecure"], "service": NOOP_SERVICE, "middlewares": [name], "tls": tls_block(cert) }),
        );
        routers.insert(format!("{router}-http"), json!({ "rule": rule, "entryPoints": ["web"], "service": NOOP_SERVICE, "middlewares": [name] }));
    } else {
        routers.insert(router.to_owned(), json!({ "rule": rule, "entryPoints": ["web"], "service": NOOP_SERVICE, "middlewares": [name] }));
    }
}

/// The body limit and compression, shared by a domain's own router and its paths.
fn domain_middlewares(middlewares: &mut Map<String, Value>, router: &str, extra: &Extra) -> Vec<String> {
    let mut chain = Vec::new();
    if let Some(bytes) = extra.max_body_bytes {
        let name = format!("{router}-body");
        middlewares.insert(name.clone(), json!({ "buffering": { "maxRequestBodyBytes": bytes } }));
        chain.push(name);
    }
    match &extra.compress {
        Some(c) if *c == Compression::default() => chain.push(COMPRESS_MIDDLEWARE.to_owned()),
        Some(c) => {
            let name = format!("{router}-compress");
            let mut settings = Map::new();
            if !c.types.is_empty() {
                settings.insert("includedContentTypes".into(), json!(c.types));
            }
            if let Some(min) = c.min_bytes {
                settings.insert("minResponseBodyBytes".into(), json!(min));
            }
            if c.gzip_only {
                settings.insert("encodings".into(), json!(["gzip"]));
            }
            middlewares.insert(name.clone(), json!({ "compress": settings }));
            chain.push(name);
        }
        None => {}
    }
    chain
}

/// The domain's shared middlewares, then the error page, then retries, so a page is only shown once the retries ran out.
fn chain(middlewares: &mut Map<String, Value>, router: &str, shared: &[String], error_page: Option<&str>, retry: bool) -> Vec<String> {
    let mut chain = shared.to_vec();
    if let Some(query) = error_page {
        let name = format!("{router}-errors");
        middlewares.insert(
            name.clone(),
            json!({ "errors": {
                "status": ["502", "504"],
                "service": crate::sites::SERVICE,
                "query": query,
                "statusRewrites": { "502": 503, "504": 503 }
            } }),
        );
        chain.push(name);
    }
    if retry {
        chain.push(RETRY_MIDDLEWARE.to_owned());
    }
    chain
}

fn load_balancer(upstreams: &[String], port: u16, pass_host: bool, transport: Option<&str>) -> Value {
    let servers: Vec<Value> = upstreams.iter().map(|u| json!({ "url": format!("http://{u}:{port}") })).collect();
    let mut balancer = json!({ "servers": servers, "passHostHeader": pass_host });
    if let Some(name) = transport {
        balancer["serversTransport"] = json!(name);
    }
    json!({ "loadBalancer": balancer })
}

fn router(rule: String, service: String, middlewares: Vec<String>, tls: Option<&RouteCert>) -> Value {
    match tls {
        Some(cert) => json!({ "rule": rule, "entryPoints": ["websecure"], "service": service, "middlewares": middlewares, "tls": tls_block(cert) }),
        None => json!({ "rule": rule, "entryPoints": ["web"], "service": service, "middlewares": middlewares }),
    }
}

pub fn render(deployment: &str, routes: &[Route]) -> Value {
    let mut routers = Map::new();
    let mut services = Map::new();
    let mut middlewares = Map::new();
    let mut transports = Map::new();
    for route in routes {
        let router_id = router_name(deployment, &route.hostname);
        let tls = route.tls.then_some(&route.cert);
        let extra = &route.extra;
        if let Some(target) = &extra.redirect {
            redirect_routers(&mut routers, &mut middlewares, &router_id, &route.hostname, tls, target);
            continue;
        }
        let transport = extra.timeout.map(|seconds| {
            let name = format!("{deployment}-t{seconds}");
            let limit = format!("{seconds}s");
            transports.insert(name.clone(), json!({ "forwardingTimeouts": { "dialTimeout": limit, "responseHeaderTimeout": limit } }));
            name
        });
        let shared = domain_middlewares(&mut middlewares, &router_id, extra);
        let rule = format!("Host(`{}`)", route.hostname);
        let service = service_key(deployment, &route.service, route.port, extra.rewrite_host, extra.timeout);
        let own = chain(&mut middlewares, &router_id, &shared, extra.error_page.as_deref(), extra.retry);
        routers.insert(router_id.clone(), router(rule.clone(), service.clone(), own, tls));
        if route.tls {
            routers.insert(
                format!("{router_id}-http"),
                json!({ "rule": rule, "entryPoints": ["web"], "service": service, "middlewares": [HTTPS_MIDDLEWARE] }),
            );
        }
        services.insert(service, load_balancer(&route.upstreams, route.port, !extra.rewrite_host, transport.as_deref()));

        for (i, path) in extra.paths.iter().enumerate() {
            let name = format!("{router_id}-p{i}");
            let matcher = if path.exact { format!("Path(`{}`)", path.path) } else { format!("PathPrefix(`{}`)", path.path) };
            let target = service_key(deployment, &path.service, path.port, false, extra.timeout);
            let middleware = chain(&mut middlewares, &name, &shared, path.error_page.as_deref(), path.retry);
            let mut value = router(format!("{rule} && {matcher}"), target.clone(), middleware, tls);
            value["priority"] = json!(PATH_PRIORITY + path.path.len() * 2 + usize::from(path.exact));
            routers.insert(name, value);
            services.insert(target, load_balancer(&path.upstreams, path.port, true, transport.as_deref()));
        }
    }
    if routers.is_empty() {
        return json!({});
    }
    let mut http = json!({ "routers": routers });
    if !services.is_empty() {
        http["services"] = Value::Object(services);
    }
    if !middlewares.is_empty() {
        http["middlewares"] = Value::Object(middlewares);
    }
    if !transports.is_empty() {
        http["serversTransports"] = Value::Object(transports);
    }
    json!({ "http": http })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(hostname: &str, tls: bool, upstreams: &[&str]) -> Route {
        Route {
            hostname: hostname.into(),
            tls,
            service: "app".into(),
            port: 8080,
            upstreams: upstreams.iter().map(|s| s.to_string()).collect(),
            cert: RouteCert::Acme { wildcard_zone: None },
            extra: Extra::default(),
        }
    }

    fn path(path: &str, exact: bool, service: &str, port: u16, error_page: Option<&str>) -> PathRoute {
        PathRoute {
            path: path.into(),
            exact,
            service: service.into(),
            port,
            upstreams: vec![format!("kuroe_lufas-{service}-1")],
            error_page: error_page.map(str::to_owned),
            retry: true,
        }
    }

    #[test]
    fn body_limits_custom_compression_timeouts_and_host_rewrites() {
        let mut lufas = route("lufas.example.com", false, &["172.18.0.7"]);
        lufas.extra = Extra {
            compress: Some(Compression { types: vec!["text/html".into()], min_bytes: Some(1024), gzip_only: true }),
            error_page: Some("/_errors/d1/dm1/default.html".into()),
            retry: false,
            max_body_bytes: Some(1024 * 1024),
            timeout: Some(60),
            rewrite_host: true,
            paths: vec![path("/api/", false, "backend", 3420, None)],
            ..Default::default()
        };
        let out = render("lufas", &[lufas]);
        let http = &out["http"];
        let own = &http["routers"]["lufas-lufas-example-com"];
        assert_eq!(
            own["middlewares"],
            json!(["lufas-lufas-example-com-body", "lufas-lufas-example-com-compress", "lufas-lufas-example-com-errors"]),
            "no retries, so the error page shows at once"
        );
        assert_eq!(own["service"], "lufas-app-8080-ip-t60");
        assert_eq!(
            http["routers"]["lufas-lufas-example-com-p0"]["middlewares"],
            json!(["lufas-lufas-example-com-body", "lufas-lufas-example-com-compress", RETRY_MIDDLEWARE]),
            "paths share the body limit and compression, and keep their own retry"
        );
        assert_eq!(http["middlewares"]["lufas-lufas-example-com-body"], json!({ "buffering": { "maxRequestBodyBytes": 1048576 } }));
        assert_eq!(
            http["middlewares"]["lufas-lufas-example-com-compress"],
            json!({ "compress": { "includedContentTypes": ["text/html"], "minResponseBodyBytes": 1024, "encodings": ["gzip"] } })
        );
        let own_service = &http["services"]["lufas-app-8080-ip-t60"]["loadBalancer"];
        assert_eq!(own_service["passHostHeader"], false);
        assert_eq!(own_service["serversTransport"], "lufas-t60");
        let api = &http["services"]["lufas-backend-3420-t60"]["loadBalancer"];
        assert_eq!(api["passHostHeader"], true, "paths always pass the visitor's host");
        assert_eq!(
            http["serversTransports"]["lufas-t60"],
            json!({ "forwardingTimeouts": { "dialTimeout": "60s", "responseHeaderTimeout": "60s" } })
        );
    }

    #[test]
    fn default_compression_uses_the_shared_middleware() {
        let mut site = route("site.test", false, &["c1"]);
        site.extra.compress = Some(Compression::default());
        let out = render("x", &[site]);
        assert_eq!(out["http"]["routers"]["x-site-test"]["middlewares"], json!([COMPRESS_MIDDLEWARE, RETRY_MIDDLEWARE]));
        assert!(out["http"].get("middlewares").is_none());
        assert!(out["http"].get("serversTransports").is_none());
    }

    #[test]
    fn paths_go_to_their_own_services_and_outrank_the_hostname() {
        let mut lufas = route("lufas.example.com", true, &["kuroe_lufas-frontend-1"]);
        lufas.service = "frontend".into();
        lufas.port = 3421;
        lufas.extra = Extra {
            compress: Some(Compression::default()),
            error_page: Some("/_errors/d1/dm1/default.html".into()),
            paths: vec![
                path("/api/", false, "backend", 3420, Some("/_errors/d1/dm1/0.json")),
                path("/robots.txt", true, "backend", 3420, None),
            ],
            ..Default::default()
        };
        let out = render("lufas", &[lufas]);
        let routers = &out["http"]["routers"];
        let own = &routers["lufas-lufas-example-com"];
        assert_eq!(own["service"], "lufas-frontend-3421");
        assert_eq!(own["middlewares"], json!([COMPRESS_MIDDLEWARE, "lufas-lufas-example-com-errors", RETRY_MIDDLEWARE]));
        assert!(own.get("priority").is_none(), "Traefik ranks the hostname's own router by its rule");

        let api = &routers["lufas-lufas-example-com-p0"];
        assert_eq!(api["rule"], "Host(`lufas.example.com`) && PathPrefix(`/api/`)");
        assert_eq!(api["service"], "lufas-backend-3420");
        assert_eq!(api["entryPoints"], json!(["websecure"]));
        assert_eq!(api["tls"], json!({ "certResolver": "le" }));
        assert_eq!(api["middlewares"], json!([COMPRESS_MIDDLEWARE, "lufas-lufas-example-com-p0-errors", RETRY_MIDDLEWARE]));
        let robots = &routers["lufas-lufas-example-com-p1"];
        assert_eq!(robots["rule"], "Host(`lufas.example.com`) && Path(`/robots.txt`)");
        assert_eq!(robots["middlewares"], json!([COMPRESS_MIDDLEWARE, RETRY_MIDDLEWARE]), "no page of its own");
        assert!(robots["priority"].as_u64().unwrap() > api["priority"].as_u64().unwrap(), "longer paths win");
        assert!(api["priority"].as_u64().unwrap() > 1000);

        let errors = &out["http"]["middlewares"]["lufas-lufas-example-com-p0-errors"]["errors"];
        assert_eq!(errors["query"], "/_errors/d1/dm1/0.json");
        assert_eq!(errors["service"], crate::sites::SERVICE);
        assert_eq!(errors["status"], json!(["502", "504"]));
        assert_eq!(errors["statusRewrites"], json!({ "502": 503, "504": 503 }));
        assert_eq!(out["http"]["services"].as_object().unwrap().len(), 2, "the two backend paths share one service");
        assert_eq!(
            out["http"]["services"]["lufas-backend-3420"]["loadBalancer"]["servers"],
            json!([{ "url": "http://kuroe_lufas-backend-1:3420" }])
        );
        assert_eq!(routers.as_object().unwrap().len(), 4, "own, its HTTP redirect, and two paths");
    }

    #[test]
    fn plain_http_paths_use_the_web_entry_point() {
        let mut site = route("plain.test", false, &["c1"]);
        site.extra.paths = vec![path("/api/", false, "api", 9000, None)];
        let out = render("x", &[site]);
        assert_eq!(out["http"]["routers"]["x-plain-test-p0"]["entryPoints"], json!(["web"]));
        assert!(out["http"]["routers"]["x-plain-test-p0"].get("tls").is_none());
        assert!(out["http"].get("middlewares").is_none(), "nothing extra without compression or error pages");
    }

    #[test]
    fn redirects_keep_the_path_and_skip_the_https_hop() {
        let mut old = route("ms.example.com", true, &[]);
        old.extra.redirect = Some("https://lufas.example.com".into());
        let out = render("lufas", &[old]);
        let routers = &out["http"]["routers"];
        assert_eq!(routers["lufas-ms-example-com"]["service"], NOOP_SERVICE);
        assert_eq!(routers["lufas-ms-example-com"]["middlewares"], json!(["lufas-ms-example-com-redirect"]));
        assert_eq!(routers["lufas-ms-example-com-http"]["middlewares"], json!(["lufas-ms-example-com-redirect"]));
        assert_eq!(
            out["http"]["middlewares"]["lufas-ms-example-com-redirect"],
            json!({ "redirectRegex": { "regex": "^https?://[^/]+(.*)$", "replacement": "https://lufas.example.com${1}", "permanent": true } })
        );
        assert!(out["http"].get("services").is_none(), "a redirect needs no service");
    }

    #[test]
    fn tls_route_gets_redirecting_http_router() {
        let out = render("shop", &[route("shop.example.com", true, &["kuroe_shop-blue-app-1"])]);
        assert_eq!(
            out,
            json!({
                "http": {
                    "routers": {
                        "shop-shop-example-com": {
                            "rule": "Host(`shop.example.com`)",
                            "entryPoints": ["websecure"],
                            "service": "shop-app-8080",
                            "middlewares": ["kuroe-retry@file"],
                            "tls": { "certResolver": "le" }
                        },
                        "shop-shop-example-com-http": {
                            "rule": "Host(`shop.example.com`)",
                            "entryPoints": ["web"],
                            "service": "shop-app-8080",
                            "middlewares": ["kuroe-https@file"]
                        }
                    },
                    "services": {
                        "shop-app-8080": {
                            "loadBalancer": {
                                "servers": [{ "url": "http://kuroe_shop-blue-app-1:8080" }],
                                "passHostHeader": true
                            }
                        }
                    }
                }
            })
        );
    }

    #[test]
    fn plain_http_route_and_replicas() {
        let out = render("api", &[route("api.local", false, &["kuroe_api-app-1", "kuroe_api-app-2"])]);
        assert_eq!(out["http"]["routers"]["api-api-local"]["entryPoints"], json!(["web"]));
        assert_eq!(out["http"]["routers"]["api-api-local"]["middlewares"], json!(["kuroe-retry@file"]));
        assert_eq!(
            out["http"]["services"]["api-app-8080"]["loadBalancer"]["servers"],
            json!([{ "url": "http://kuroe_api-app-1:8080" }, { "url": "http://kuroe_api-app-2:8080" }])
        );
    }

    #[test]
    fn two_domains_share_one_service() {
        let out = render("web", &[route("a.test", false, &["c1"]), route("b.test", false, &["c1"])]);
        assert_eq!(out["http"]["routers"].as_object().unwrap().len(), 2);
        assert_eq!(out["http"]["services"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn saved_certificates_drop_the_resolver_and_wildcards_ask_for_the_zone() {
        let mut saved = route("app.example.com", true, &["c1"]);
        saved.cert = RouteCert::Saved { id: "origin".into(), name: "origin".into(), aop: false };
        let mut pulled = route("api.example.com", true, &["c1"]);
        pulled.cert = RouteCert::Saved { id: "origin".into(), name: "origin".into(), aop: true };
        let mut wild = route("shop.other.dev", true, &["c1"]);
        wild.cert = RouteCert::Acme { wildcard_zone: Some("other.dev".into()) };
        let out = render("x", &[saved, pulled, wild]);
        let routers = &out["http"]["routers"];
        assert_eq!(routers["x-app-example-com"]["tls"], json!({}));
        assert_eq!(routers["x-api-example-com"]["tls"], json!({ "options": "kuroe-aop@file" }));
        assert_eq!(
            routers["x-shop-other-dev"]["tls"],
            json!({ "certResolver": "le", "domains": [{ "main": "other.dev", "sans": ["*.other.dev"] }] })
        );
    }

    #[test]
    fn no_routes_is_empty_config() {
        assert_eq!(render("x", &[]), json!({}));
    }

    #[test]
    fn stopped_deployment_has_no_servers() {
        let out = render("x", &[route("x.test", false, &[])]);
        assert_eq!(out["http"]["services"]["x-app-8080"]["loadBalancer"]["servers"], json!([]));
    }
}
