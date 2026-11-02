use std::time::Duration;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, LogsOptionsBuilder, RemoveContainerOptionsBuilder,
    UploadToContainerOptionsBuilder, WaitContainerOptionsBuilder,
};
use futures::StreamExt;
use kuroe::proxy::certs::{self, RouteCert};
use kuroe::proxy::routes::{self, Route};

const TRAEFIK: &str = "kuroe_test_traefik";
const CHECKER: &str = "kuroe_test_traefik_check";
const CHECK_IMAGE: &str = "rust:1.99-slim";

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

async fn upload(docker: &Docker, container: &str, path: &str, files: &[(&str, &[u8])]) {
    let options = UploadToContainerOptionsBuilder::new().path(path).build();
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

const CHECK: &str = r#"
req() { h=$1; shift; printf 'GET / HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n' "$h" | timeout 5 openssl s_client -quiet -connect 127.0.0.1:443 -servername "$h" "$@" 2>&1; }
echo "== served"
echo | timeout 5 openssl s_client -connect 127.0.0.1:443 -servername app.example.com 2>/dev/null | openssl x509 -noout -ext subjectAltName
echo "== plain $(req app.example.com | grep -o 'HTTP/1.1 [0-9]*' | head -1)"
echo "== aop without client cert $(req api.example.com | grep -o 'HTTP/1.1 [0-9]*\|alert[a-z ]*' | head -1)"
echo "== aop with client cert $(req api.example.com -cert /check/client.crt -key /check/client.key | grep -o 'HTTP/1.1 [0-9]*' | head -1)"
"#;

#[tokio::test]
#[ignore = "needs a reachable Docker daemon"]
async fn traefik_serves_saved_certificates_and_enforces_origin_pulls() {
    let docker = kuroe::docker::connect().unwrap();
    pull(&docker, kuroe::proxy::traefik::IMAGE).await;
    pull(&docker, CHECK_IMAGE).await;
    remove(&docker, CHECKER).await;
    remove(&docker, TRAEFIK).await;

    let server_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let server = rcgen::CertificateParams::new(vec!["example.com".into(), "*.example.com".into()]).unwrap().self_signed(&server_key).unwrap();
    let ca_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::from_params(&ca_params, &ca_key);
    let client_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let client = rcgen::CertificateParams::new(vec!["origin-pull.test".into()]).unwrap().signed_by(&client_key, &issuer).unwrap();

    let saved = certs::Saved {
        id: "origin".into(),
        name: "origin".into(),
        sans: vec!["example.com".into(), "*.example.com".into()],
        not_after: 0,
        aop: true,
        chain: server.pem(),
        key: server_key.serialize_pem(),
    };
    let route = |host: &str, aop: bool| Route {
        hostname: host.into(),
        tls: true,
        service: "app".into(),
        port: 8080,
        upstreams: vec!["127.0.0.1".into()],
        cert: RouteCert::Saved { id: "origin".into(), name: "origin".into(), aop },
        extra: Default::default(),
    };
    let dynamic = routes::render("t", &[route("app.example.com", false), route("api.example.com", true)]);
    let store = certs::store_config(&[&saved]);

    let body = ContainerCreateBody {
        image: Some(kuroe::proxy::traefik::IMAGE.into()),
        cmd: Some(
            [
                "--providers.file.directory=/etc/traefik/dynamic",
                "--entrypoints.websecure.address=:443",
                "--entrypoints.websecure.http.tls=true",
                "--log.level=INFO",
                "--api.insecure=true",
            ]
            .map(String::from)
            .to_vec(),
        ),
        ..Default::default()
    };
    docker.create_container(Some(CreateContainerOptionsBuilder::new().name(TRAEFIK).build()), body).await.unwrap();
    let crt = saved.chain.clone();
    let key = saved.key.clone();
    let ca_pem = ca.pem();
    upload(
        &docker,
        TRAEFIK,
        "/",
        &[
            ("etc/traefik/dynamic/_kuroe.yml", routes::shared_config().to_string().as_bytes()),
            ("etc/traefik/dynamic/t.yml", dynamic.to_string().as_bytes()),
            ("etc/traefik/dynamic/_certs.yml", store.to_string().as_bytes()),
            ("etc/traefik/certs/origin.crt", crt.as_bytes()),
            ("etc/traefik/certs/origin.key", key.as_bytes()),
            (&format!("etc/traefik/certs/{}", certs::AOP_CA_FILE), ca_pem.as_bytes()),
        ],
    )
    .await;
    docker.start_container(TRAEFIK, None).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    let checker = ContainerCreateBody {
        image: Some(CHECK_IMAGE.into()),
        cmd: Some(vec!["sh".into(), "-c".into(), CHECK.into()]),
        host_config: Some(HostConfig { network_mode: Some(format!("container:{TRAEFIK}")), ..Default::default() }),
        ..Default::default()
    };
    docker.create_container(Some(CreateContainerOptionsBuilder::new().name(CHECKER).build()), checker).await.unwrap();
    let client_crt = client.pem();
    let client_key_pem = client_key.serialize_pem();
    upload(&docker, CHECKER, "/", &[("check/client.crt", client_crt.as_bytes()), ("check/client.key", client_key_pem.as_bytes())]).await;
    docker.start_container(CHECKER, None).await.unwrap();
    let mut wait = docker.wait_container(CHECKER, Some(WaitContainerOptionsBuilder::new().build()));
    while wait.next().await.is_some() {}
    let mut logs = docker.logs(CHECKER, Some(LogsOptionsBuilder::new().stdout(true).stderr(true).build()));
    let mut out = String::new();
    while let Some(Ok(chunk)) = logs.next().await {
        out.push_str(&chunk.to_string());
    }
    remove(&docker, CHECKER).await;
    remove(&docker, TRAEFIK).await;

    assert!(out.contains("DNS:example.com, DNS:*.example.com"), "the saved certificate is served for a subdomain:\n{out}");
    assert!(out.contains("== plain HTTP/1.1 "), "a route without origin pulls answers without a client cert:\n{out}");
    assert!(!out.contains("== aop without client cert HTTP/1.1"), "origin pulls refuse a client without a certificate:\n{out}");
    assert!(out.contains("== aop with client cert HTTP/1.1 "), "origin pulls let Cloudflare's certificate through:\n{out}");
}
