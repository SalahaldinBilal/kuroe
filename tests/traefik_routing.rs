use std::time::Duration;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig, NetworkCreateRequest};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, LogsOptionsBuilder, RemoveContainerOptionsBuilder,
    UploadToContainerOptionsBuilder, WaitContainerOptionsBuilder,
};
use futures::StreamExt;
use kuroe::proxy::certs::RouteCert;
use kuroe::proxy::routes::{self, Compression, Extra, PathRoute, Route};

const NETWORK: &str = "kuroe_test_routing";
const TRAEFIK: &str = "kuroe_test_routing_traefik";
const STATIC: &str = "kuroe_test_routing_static";
const CHECKER: &str = "kuroe_test_routing_check";
const CHECK_IMAGE: &str = "rust:1.99-slim";
const UPSTREAMS: [(&str, &str); 4] = [
    ("kuroe_test_routing_frontend", "return 200 \"frontend\\n\";"),
    ("kuroe_test_routing_backend", "return 200 \"backend $host\\n\";"),
    ("kuroe_test_routing_broken", "return 502;"),
    ("kuroe_test_routing_hostcheck", "return 200 \"host $host\\n\";"),
];

fn tar(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, body) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, *body).unwrap();
    }
    builder.into_inner().unwrap()
}

async fn upload(docker: &Docker, container: &str, files: &[(&str, &[u8])]) {
    let options = UploadToContainerOptionsBuilder::new().path("/").build();
    docker.upload_to_container(container, Some(options), bollard::body_full(tar(files).into())).await.unwrap();
}

async fn pull(docker: &Docker, image: &str) {
    if docker.inspect_image(image).await.is_err() {
        let mut stream = docker.create_image(Some(CreateImageOptionsBuilder::new().from_image(image).build()), None, None);
        while let Some(step) = stream.next().await {
            step.unwrap();
        }
    }
}

async fn remove(docker: &Docker, name: &str) {
    let _ = docker.remove_container(name, Some(RemoveContainerOptionsBuilder::new().force(true).build())).await;
}

async fn create(docker: &Docker, name: &str, image: &str, cmd: Option<Vec<String>>) {
    let body = ContainerCreateBody {
        image: Some(image.into()),
        cmd,
        host_config: Some(HostConfig { network_mode: Some(NETWORK.into()), ..Default::default() }),
        ..Default::default()
    };
    docker.create_container(Some(CreateContainerOptionsBuilder::new().name(name).build()), body).await.unwrap();
}

async fn clean(docker: &Docker) {
    for name in [CHECKER, TRAEFIK, STATIC] {
        remove(docker, name).await;
    }
    for (name, _) in UPSTREAMS {
        remove(docker, name).await;
    }
    let _ = docker.remove_network(NETWORK).await;
}

/// Raw HTTP over bash's /dev/tcp, since the image has no wget or curl. Prints the status line, headers and body.
const CHECK: &str = r#"
get() { exec 3<>"/dev/tcp/127.0.0.1/${3:-80}"; printf 'GET %s HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n' "$2" "$1" >&3; timeout 5 cat <&3; exec 3<&-; echo; }
post() { body=$(head -c "$3" /dev/zero | tr '\0' a); exec 3<>/dev/tcp/127.0.0.1/80; printf 'POST %s HTTP/1.1\r\nHost: %s\r\nContent-Length: %s\r\nConnection: close\r\n\r\n%s' "$2" "$1" "$3" "$body" >&3; timeout 5 cat <&3; exec 3<&-; echo; }
echo "== root"; get lufas.test /
echo "== api"; get lufas.test /api/x
echo "== apix"; get lufas.test /apix
echo "== broken"; get lufas.test /broken
echo "== small"; post lufas.test /api/x 500
echo "== big"; post lufas.test /api/x 2000
echo "== rewritten"; get ip.test /
echo "== old"; get old.test '/a?b=1'
echo "== routers"; get localhost /api/http/routers 8080
echo "== middlewares"; get localhost /api/http/middlewares 8080
echo "== services"; get localhost /api/http/services 8080
"#;

fn section<'a>(out: &'a str, name: &str) -> &'a str {
    let marker = format!("== {name}\n");
    let start = out.find(&marker).map(|i| i + marker.len()).unwrap_or(out.len());
    let rest = &out[start..];
    &rest[..rest.find("\n== ").unwrap_or(rest.len())]
}

#[tokio::test]
#[ignore = "needs a reachable Docker daemon"]
async fn traefik_routes_paths_redirects_and_error_pages() {
    let docker = kuroe::docker::connect().unwrap();
    pull(&docker, kuroe::proxy::traefik::IMAGE).await;
    pull(&docker, kuroe::sites::IMAGE).await;
    pull(&docker, CHECK_IMAGE).await;
    clean(&docker).await;
    docker.create_network(NetworkCreateRequest { name: NETWORK.into(), ..Default::default() }).await.unwrap();

    for (name, answer) in UPSTREAMS {
        create(&docker, name, kuroe::sites::IMAGE, None).await;
        let conf = format!("server {{ listen 80; location / {{ {answer} }} }}\n");
        upload(&docker, name, &[("etc/nginx/conf.d/default.conf", conf.as_bytes())]).await;
        docker.start_container(name, None).await.unwrap();
    }
    create(&docker, STATIC, kuroe::sites::IMAGE, None).await;
    let static_conf = kuroe::sites::nginx_conf(&Default::default(), &Default::default()).unwrap();
    upload(
        &docker,
        STATIC,
        &[
            ("etc/nginx/nginx.conf", static_conf.as_bytes()),
            ("sites/_errors/d1/dm1/default.html", b"back soon"),
            ("sites/_errors/d1/dm1/1.json", br#"{"error":"down"}"#),
        ],
    )
    .await;
    docker.start_container(STATIC, None).await.unwrap();

    let path = |path: &str, exact: bool, upstream: &str, error_page: Option<&str>| PathRoute {
        path: path.into(),
        exact,
        service: upstream.rsplit('_').next().unwrap().into(),
        port: 80,
        upstreams: vec![upstream.into()],
        retry: error_page.is_none(),
        error_page: error_page.map(str::to_owned),
    };
    let lufas = Route {
        hostname: "lufas.test".into(),
        tls: false,
        service: "frontend".into(),
        port: 80,
        upstreams: vec![UPSTREAMS[0].0.into()],
        cert: RouteCert::Default,
        extra: Extra {
            compress: Some(Compression { types: vec!["text/html".into(), "text/plain".into()], min_bytes: Some(1024), gzip_only: true }),
            error_page: Some("/_errors/d1/dm1/default.html".into()),
            retry: false,
            max_body_bytes: Some(1000),
            timeout: Some(60),
            paths: vec![
                path("/api/", false, UPSTREAMS[1].0, None),
                path("/broken", true, UPSTREAMS[2].0, Some("/_errors/d1/dm1/1.json")),
            ],
            ..Default::default()
        },
    };
    let rewritten = Route {
        hostname: "ip.test".into(),
        tls: false,
        service: "hostcheck".into(),
        port: 80,
        upstreams: vec![UPSTREAMS[3].0.into()],
        cert: RouteCert::Default,
        extra: Extra { rewrite_host: true, ..Default::default() },
    };
    let old = Route {
        hostname: "old.test".into(),
        tls: false,
        service: String::new(),
        port: 0,
        upstreams: Vec::new(),
        cert: RouteCert::Default,
        extra: Extra { redirect: Some("https://lufas.test".into()), ..Default::default() },
    };
    let mut shared = routes::shared_config();
    shared["http"]["services"][kuroe::sites::SERVICE]["loadBalancer"]["servers"][0]["url"] = format!("http://{STATIC}:80").into();
    let dynamic = routes::render("t", &[lufas, old, rewritten]);

    let cmd = ["--providers.file.directory=/etc/traefik/dynamic", "--entrypoints.web.address=:80", "--api.insecure=true", "--log.level=INFO"];
    create(&docker, TRAEFIK, kuroe::proxy::traefik::IMAGE, Some(cmd.map(String::from).to_vec())).await;
    upload(
        &docker,
        TRAEFIK,
        &[
            ("etc/traefik/dynamic/_kuroe.yml", shared.to_string().as_bytes()),
            ("etc/traefik/dynamic/t.yml", dynamic.to_string().as_bytes()),
        ],
    )
    .await;
    docker.start_container(TRAEFIK, None).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    let checker = ContainerCreateBody {
        image: Some(CHECK_IMAGE.into()),
        cmd: Some(vec!["bash".into(), "-c".into(), CHECK.into()]),
        host_config: Some(HostConfig { network_mode: Some(format!("container:{TRAEFIK}")), ..Default::default() }),
        ..Default::default()
    };
    docker.create_container(Some(CreateContainerOptionsBuilder::new().name(CHECKER).build()), checker).await.unwrap();
    docker.start_container(CHECKER, None).await.unwrap();
    let mut wait = docker.wait_container(CHECKER, Some(WaitContainerOptionsBuilder::new().build()));
    while wait.next().await.is_some() {}
    let mut logs = docker.logs(CHECKER, Some(LogsOptionsBuilder::new().stdout(true).stderr(true).build()));
    let mut out = String::new();
    while let Some(Ok(chunk)) = logs.next().await {
        out.push_str(&chunk.to_string());
    }
    clean(&docker).await;

    assert!(section(&out, "root").contains("frontend"), "the hostname goes to its own service:\n{out}");
    assert!(section(&out, "api").contains("backend lufas.test"), "a prefix path goes to its service with the visitor's host:\n{out}");
    assert!(section(&out, "apix").contains("frontend"), "/apix is not under /api/:\n{out}");
    let broken = section(&out, "broken");
    assert!(broken.contains(" 503 ") && broken.contains(r#"{"error":"down"}"#), "a 502 becomes the path's own page as a 503:\n{out}");
    assert!(broken.contains("Retry-After: 120") && broken.contains("Cache-Control: no-store"), "the page keeps its headers:\n{out}");
    assert!(section(&out, "small").contains("backend"), "a request under the limit passes:\n{out}");
    assert!(section(&out, "big").contains(" 413 "), "a request over the limit is refused:\n{out}");
    assert!(section(&out, "rewritten").contains("host kuroe_test_routing_hostcheck"), "the service sees its own address:\n{out}");
    let old = section(&out, "old");
    assert!(old.contains(" 301 ") && old.contains("Location: https://lufas.test/a?b=1"), "redirects keep the path and query:\n{out}");
    let routers = section(&out, "routers");
    assert!(routers.contains("t-lufas-test-p1@file") && routers.contains("t-old-test@file"), "every router loaded:\n{out}");
    for kind in ["routers", "middlewares", "services"] {
        assert!(!section(&out, kind).contains("\"disabled\""), "Traefik took every one of the {kind}:\n{out}");
    }
    assert!(section(&out, "services").contains("t-backend-80-t60@file"), "services carry the timeout:\n{out}");
}
