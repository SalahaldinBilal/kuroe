use serde_json::{Map, Value, json};

use super::{Entry, Mode, PROBE_HEADER};

fn name(hostname: &str) -> String {
    format!("kuroe-{}", hostname.replace('.', "-"))
}

/// A file for the user's own Traefik file provider. Entry points are left out so the routers listen everywhere.
pub fn file(entries: &[Entry], mode: Mode, traefik_tls: &str) -> String {
    let mut routers = Map::new();
    let mut services = Map::new();
    let mut middlewares = Map::new();
    let mut tcp_routers = Map::new();
    for e in entries {
        let n = name(&e.hostname);
        let mut router = json!({ "rule": format!("Host(`{}`)", e.hostname), "service": n });
        if let Some(token) = &e.probe {
            middlewares.insert(format!("{n}-probe"), json!({ "headers": { "customResponseHeaders": { PROBE_HEADER: token } } }));
            router["middlewares"] = json!([format!("{n}-probe")]);
        }
        if e.tls && mode == Mode::Front {
            router["tls"] = json!({ "certResolver": "letsencrypt" });
        }
        routers.insert(n.clone(), router);
        services.insert(n.clone(), json!({ "loadBalancer": { "servers": [{ "url": format!("http://{}", e.upstream) }] } }));
        if mode == Mode::Passthrough && e.tls {
            tcp_routers.insert(n, json!({ "rule": format!("HostSNI(`{}`)", e.hostname), "service": "kuroe-traefik-tls", "tls": { "passthrough": true } }));
        }
    }
    let mut http = json!({ "routers": routers, "services": services });
    if !middlewares.is_empty() {
        http["middlewares"] = Value::Object(middlewares);
    }
    let mut config = json!({ "http": http });
    if !tcp_routers.is_empty() {
        config["tcp"] = json!({
            "routers": tcp_routers,
            "services": { "kuroe-traefik-tls": { "loadBalancer": { "servers": [{ "address": traefik_tls }] } } },
        });
    }
    format!("# Written by kuroe. Changes here are overwritten.\n{}\n", serde_json::to_string_pretty(&config).expect("json"))
}

#[cfg(test)]
mod tests {
    use super::super::tests::entries;
    use super::*;

    fn parse(out: &str) -> Value {
        serde_json::from_str(out.split_once('\n').unwrap().1).unwrap()
    }

    #[test]
    fn http_routers_and_sni_passthrough() {
        let front = parse(&file(&entries(true), Mode::Front, "127.0.0.1:8443"));
        assert_eq!(front["http"]["routers"]["kuroe-shop-example-com"]["tls"], json!({ "certResolver": "letsencrypt" }));
        assert_eq!(front["http"]["middlewares"]["kuroe-api-example-com-probe"]["headers"]["customResponseHeaders"]["x-kuroe-probe"], "p2");
        assert!(front.get("tcp").is_none());

        let pass = parse(&file(&entries(false), Mode::Passthrough, "127.0.0.1:8443"));
        assert!(pass["http"]["routers"]["kuroe-shop-example-com"].get("tls").is_none(), "Traefik behind does TLS");
        assert_eq!(pass["tcp"]["routers"]["kuroe-shop-example-com"]["rule"], "HostSNI(`shop.example.com`)");
        assert_eq!(pass["tcp"]["routers"]["kuroe-shop-example-com"]["tls"], json!({ "passthrough": true }));
        assert_eq!(pass["tcp"]["services"]["kuroe-traefik-tls"]["loadBalancer"]["servers"][0]["address"], "127.0.0.1:8443");
    }
}
