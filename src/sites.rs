//! Static sites: one shared nginx container serves every site from the `kuroe_sites` volume.
//! Files live at `/sites/<deployment id>/<revision>/`, and Traefik routes each hostname there with an `addPrefix`.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, HostConfig, RestartPolicy, RestartPolicyNameEnum, VolumeCreateRequest};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, DownloadFromContainerOptionsBuilder, LogsOptionsBuilder,
    RemoveContainerOptionsBuilder, UploadToContainerOptionsBuilder,
};
use futures::StreamExt;
use serde_json::{Map, Value, json};
use sqlx::SqlitePool;

use crate::docker::rewrite::{LABEL_MANAGED, PROXY_NETWORK};
use crate::proxy::certs::RouteCert;
use crate::proxy::options::ERRORS_DIR;
use crate::proxy::routes::{HTTPS_MIDDLEWARE, redirect_routers, router_name, tls_block};

pub const CONTAINER: &str = "kuroe_static";
/// The official image, Alpine for the shell that installing a revision needs.
pub const IMAGE: &str = "nginx:1.28-alpine";
pub const VOLUME: &str = "kuroe_sites";
pub const SERVICE: &str = "kuroe-static";
/// Traefik sets this to `1` for single-page apps and clears it for every other site.
pub const SPA_HEADER: &str = "X-Kuroe-Spa";
const ROOT: &str = "/sites";
const PROJECT_PREFIX: &str = "site/";
const BUILD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const LABEL_SITE_BUILD: &str = "kuroe.site_build";
const LABEL_IMAGE: &str = "kuroe.image";

pub const MIME_TYPES_SETTING: &str = "static_mime_types";
/// Types the stock `mime.types` of the image lacks. The setting can change them.
const DEFAULT_MIME_TYPES: &[(&str, &str)] = &[("mjs", "application/javascript")];
const MIME_TYPES_MARK: &str = "        __MIME_TYPES__\n";
const SITE_LOCATIONS_MARK: &str = "        __SITE_LOCATIONS__\n";

/// File extension (without the dot) to content type.
pub type MimeTypes = BTreeMap<String, String>;

pub fn parse_mime_types(value: &str) -> Result<MimeTypes, String> {
    if value.trim().is_empty() {
        return Ok(MimeTypes::new());
    }
    let types: MimeTypes = serde_json::from_str(value).map_err(|_| "file types must be a JSON object of extension to content type".to_owned())?;
    check_mime_types(&types)?;
    Ok(types)
}

/// Every entry ends up in the nginx config, so anything but a plain extension and a plain `type/subtype` is refused.
pub fn check_mime_types(types: &MimeTypes) -> Result<(), String> {
    for (ext, content_type) in types {
        let ext_ok = !ext.is_empty()
            && ext.len() <= 32
            && ext.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '+'));
        if !ext_ok {
            return Err(format!("{ext:?} isn't a file extension. Use lowercase letters and digits, without the dot"));
        }
        if !valid_content_type(content_type) {
            return Err(format!("{content_type:?} isn't a content type like text/plain"));
        }
    }
    Ok(())
}

fn valid_content_type(value: &str) -> bool {
    let token = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "!#$&^_.+-".contains(c));
    value.len() <= 127 && value.split_once('/').is_some_and(|(kind, sub)| token(kind) && token(sub))
}

pub async fn load_mime_types(db: &SqlitePool) -> anyhow::Result<MimeTypes> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = ?").bind(MIME_TYPES_SETTING).fetch_optional(db).await?;
    parse_mime_types(value.as_deref().unwrap_or_default()).map_err(|e| anyhow!("the {MIME_TYPES_SETTING} setting: {e}"))
}

/// Static sites with their own file types, by deployment id.
pub async fn load_site_mime_types(db: &SqlitePool) -> anyhow::Result<BTreeMap<String, MimeTypes>> {
    Ok(crate::deploy::list(db)
        .await?
        .into_iter()
        .filter_map(|d| Some((d.id, d.spec.site?.mime_types)))
        .filter(|(_, types)| !types.is_empty())
        .collect())
}

pub async fn load_nginx_conf(db: &SqlitePool) -> anyhow::Result<String> {
    nginx_conf(&load_mime_types(db).await?, &load_site_mime_types(db).await?)
}

fn types_lines(layers: &[&MimeTypes], indent: &str) -> String {
    let mut merged: MimeTypes = DEFAULT_MIME_TYPES.iter().map(|(ext, t)| ((*ext).to_owned(), (*t).to_owned())).collect();
    for layer in layers {
        merged.extend((*layer).clone());
    }
    merged.iter().map(|(ext, t)| format!("{indent}{t} {ext};\n")).collect()
}

/// The image's `mime.types`, then the defaults, then `global`. A site in `sites` gets its own location, because a
/// `types` block inside a location replaces the whole list instead of adding to it.
pub fn nginx_conf(global: &MimeTypes, sites: &BTreeMap<String, MimeTypes>) -> anyhow::Result<String> {
    let mut locations = String::new();
    for (id, types) in sites {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            bail!("deployment id {id:?} can't be used in the static server config");
        }
        check_mime_types(types).map_err(|e| anyhow!("deployment {id}: {e}"))?;
        locations.push_str(&format!(
            "        location ~ ^/(?<site>{id})/(?<rev>[0-9]+)/ {{\n            include /etc/nginx/mime.types;\n            types {{\n{}            }}\n            {SITE_FILES}\n        }}\n\n",
            types_lines(&[global, types], "                "),
        ));
    }
    Ok(NGINX_CONF
        .replace(MIME_TYPES_MARK, &types_lines(&[global], "        "))
        .replace(SITE_LOCATIONS_MARK, &locations))
}

const SITE_FILES: &str = "try_files $uri $uri/index.html /$site/$rev/$kuroe_fallback =404;";

/// One config for every site. A path under `/<site>/<revision>/` is a file, a folder's `index.html`, or for a
/// single-page app the revision's `index.html` with status 200. Other sites get a plain 404.
const NGINX_CONF: &str = r#"worker_processes 2;
error_log stderr warn;
pid /run/nginx.pid;

events {
    worker_connections 4096;
}

http {
    include /etc/nginx/mime.types;
    types {
        __MIME_TYPES__
    }
    default_type application/octet-stream;
    access_log off;
    server_tokens off;
    sendfile on;
    tcp_nopush on;
    keepalive_timeout 65;
    gzip on;
    gzip_min_length 1024;
    gzip_types text/plain text/css text/xml application/javascript application/json application/xml image/svg+xml application/wasm;

    map $http_x_kuroe_spa $kuroe_fallback {
        "1" "index.html";
        default "kuroe-no-fallback";
    }

    server {
        listen 80 default_server;
        root /sites;
        autoindex off;

        location = /kuroe-health {
            return 200 "ok\n";
        }

        location ^~ /_errors/ {
            add_header Retry-After 120 always;
            add_header Cache-Control "no-store" always;
            try_files $uri =404;
        }

        location ~ /\. {
            return 404;
        }

        __SITE_LOCATIONS__
        location ~ ^/(?<site>[^/]+)/(?<rev>[0-9]+)/ {
            try_files $uri $uri/index.html /$site/$rev/$kuroe_fallback =404;
        }

        location / {
            return 404;
        }
    }
}
"#;

/// The active revision is kept in `deployments.active_project` as `site/<revision>`.
pub fn project(revision: i64) -> String {
    format!("{PROJECT_PREFIX}{revision}")
}

pub fn revision_of(project: &str) -> Option<i64> {
    project.strip_prefix(PROJECT_PREFIX)?.parse().ok()
}

pub fn cache_volume(deployment_id: &str) -> String {
    format!("kuroe_sitecache_{deployment_id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Tar,
    Gzip,
    Bzip2,
    Xz,
}

/// What Docker's archive upload accepts: plain tar, or tar compressed with gzip, bzip2 or xz.
pub fn archive_kind(bytes: &[u8]) -> Option<ArchiveKind> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        Some(ArchiveKind::Gzip)
    } else if bytes.starts_with(b"BZh") {
        Some(ArchiveKind::Bzip2)
    } else if bytes.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        Some(ArchiveKind::Xz)
    } else if bytes.get(257..262) == Some(b"ustar") {
        Some(ArchiveKind::Tar)
    } else {
        None
    }
}

pub fn service_config() -> Value {
    json!({ "loadBalancer": { "servers": [{ "url": format!("http://{CONTAINER}:80") }], "passHostHeader": true } })
}

pub struct SiteRoute {
    pub hostname: String,
    pub tls: bool,
    pub cert: RouteCert,
    pub redirect: Option<String>,
}

/// Routers for every hostname of one site, pointing at the active revision. Empty when the site isn't running.
pub fn routes(name: &str, deployment_id: &str, revision: Option<i64>, spa: bool, hosts: &[SiteRoute]) -> Value {
    let Some(revision) = revision.filter(|_| !hosts.is_empty()) else { return json!({}) };
    let base = format!("/{deployment_id}/{revision}");
    let prefix = format!("{name}-site");
    let spa_header = format!("{name}-spa");
    let mut middlewares = Map::new();
    middlewares.insert(prefix.clone(), json!({ "addPrefix": { "prefix": base } }));
    // An empty value removes the header, so a client can't turn the fallback on for a site that isn't a single-page app.
    middlewares.insert(
        spa_header.clone(),
        json!({ "headers": { "customRequestHeaders": { (SPA_HEADER): if spa { "1" } else { "" } } } }),
    );
    let chain = vec![prefix, spa_header];
    let mut routers = Map::new();
    for host in hosts {
        let router = router_name(name, &host.hostname);
        if let Some(target) = &host.redirect {
            redirect_routers(&mut routers, &mut middlewares, &router, &host.hostname, host.tls.then_some(&host.cert), target);
            continue;
        }
        let rule = format!("Host(`{}`)", host.hostname);
        if host.tls {
            routers.insert(
                router.clone(),
                json!({ "rule": rule, "entryPoints": ["websecure"], "service": SERVICE, "middlewares": chain, "tls": tls_block(&host.cert) }),
            );
            routers.insert(
                format!("{router}-http"),
                json!({ "rule": rule, "entryPoints": ["web"], "service": SERVICE, "middlewares": [HTTPS_MIDDLEWARE] }),
            );
        } else {
            routers.insert(router, json!({ "rule": rule, "entryPoints": ["web"], "service": SERVICE, "middlewares": chain }));
        }
    }
    json!({ "http": { "routers": routers, "middlewares": middlewares } })
}

fn file_tar(name: &str, body: &str) -> anyhow::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(crate::db::now() as u64);
    header.set_cksum();
    builder.append_data(&mut header, name, body.as_bytes())?;
    Ok(builder.into_inner()?)
}

fn files_tar(files: &[(String, &str)]) -> anyhow::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, body) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(crate::db::now() as u64);
        header.set_cksum();
        builder.append_data(&mut header, path, body.as_bytes())?;
    }
    Ok(builder.into_inner()?)
}

/// A tar with only folders, so an archive can be unpacked into a path that didn't exist yet.
fn folders_tar(paths: &[&str]) -> anyhow::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for path in paths {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_mtime(crate::db::now() as u64);
        header.set_cksum();
        builder.append_data(&mut header, format!("{path}/"), std::io::empty())?;
    }
    Ok(builder.into_inner()?)
}

/// Removes symlinks, then makes the output folder the revision folder. Without an output folder and with `HOIST=1`,
/// a lone top folder with no `index.html` next to it is unpacked from inside. Prints the number of files.
const NORMALIZE: &str = r#"set -e
cd "$DIR"
find . -type l -exec rm -f {} +
if [ -z "$OUT" ] && [ "$HOIST" = 1 ] && [ ! -e index.html ] && [ "$(ls -A | wc -l)" -eq 1 ]; then
  only=$(ls -A)
  if [ -d "$only" ]; then OUT=$only; fi
fi
if [ -n "$OUT" ]; then
  if [ ! -d "$DIR/$OUT" ]; then echo "the output folder $OUT is not in the files"; exit 3; fi
  rm -rf "$DIR.out"
  mv "$DIR/$OUT" "$DIR.out"
  cd /
  rm -rf "$DIR"
  mv "$DIR.out" "$DIR"
fi
find "$DIR" -type f | wc -l
"#;

#[derive(Clone)]
pub struct Sites {
    pub docker: Docker,
    pub db: SqlitePool,
}

impl Sites {
    async fn pull(&self, image: &str) -> anyhow::Result<()> {
        if self.docker.inspect_image(image).await.is_ok() {
            return Ok(());
        }
        let mut pull = self.docker.create_image(Some(CreateImageOptionsBuilder::new().from_image(image).build()), None, None);
        while let Some(step) = pull.next().await {
            step.with_context(|| format!("pulling {image}"))?;
        }
        Ok(())
    }

    /// Creates the shared server on first use and starts it again when it was stopped. A server from another image is
    /// replaced, and one with an older config gets the new file and a graceful reload.
    pub async fn ensure(&self) -> anyhow::Result<()> {
        let conf = load_nginx_conf(&self.db).await?;
        if let Ok(existing) = self.docker.inspect_container(CONTAINER, None).await {
            let image = existing.config.as_ref().and_then(|c| c.labels.as_ref()).and_then(|l| l.get(LABEL_IMAGE).cloned());
            if image.as_deref() == Some(IMAGE) {
                if existing.state.and_then(|s| s.running) != Some(true) {
                    self.docker.start_container(CONTAINER, None).await.context("starting the static file server")?;
                }
                return self.sync_config(&conf).await;
            }
            let remove = RemoveContainerOptionsBuilder::new().force(true).build();
            self.docker.remove_container(CONTAINER, Some(remove)).await.context("replacing the static file server")?;
        }
        self.pull(IMAGE).await?;
        let labels = HashMap::from([(LABEL_MANAGED.to_owned(), "true".to_owned())]);
        if self.docker.inspect_volume(VOLUME).await.is_err() {
            self.docker
                .create_volume(VolumeCreateRequest { name: Some(VOLUME.into()), labels: Some(labels.clone()), ..Default::default() })
                .await
                .context("creating the kuroe_sites volume")?;
        }
        let mut container_labels = labels;
        container_labels.insert(LABEL_IMAGE.into(), IMAGE.into());
        let body = ContainerCreateBody {
            image: Some(IMAGE.into()),
            labels: Some(container_labels),
            host_config: Some(HostConfig {
                binds: Some(vec![format!("{VOLUME}:{ROOT}")]),
                network_mode: Some(PROXY_NETWORK.into()),
                restart_policy: Some(RestartPolicy { name: Some(RestartPolicyNameEnum::UNLESS_STOPPED), maximum_retry_count: None }),
                nano_cpus: Some(1_000_000_000),
                memory: Some(256 * 1024 * 1024),
                ..Default::default()
            }),
            ..Default::default()
        };
        let options = CreateContainerOptionsBuilder::new().name(CONTAINER).build();
        self.docker.create_container(Some(options), body).await.context("creating the static file server")?;
        self.upload(CONTAINER, "/etc/nginx", file_tar("nginx.conf", &conf)?).await?;
        self.docker.start_container(CONTAINER, None).await.context("starting the static file server")?;
        Ok(())
    }

    async fn sync_config(&self, conf: &str) -> anyhow::Result<()> {
        let current = self.exec(CONTAINER, "cat /etc/nginx/nginx.conf", Vec::new()).await.unwrap_or_default();
        if current.trim() != conf.trim() {
            self.upload(CONTAINER, "/etc/nginx", file_tar("nginx.conf", conf)?).await?;
            self.exec(CONTAINER, "nginx -t -q && nginx -s reload", Vec::new()).await.context("reloading the static file server")?;
        }
        Ok(())
    }

    /// Gives a running server the current config. A stopped or missing one gets it from `ensure` when it's next needed.
    pub async fn refresh_config(&self) -> anyhow::Result<()> {
        let running = self.docker.inspect_container(CONTAINER, None).await.ok().and_then(|c| c.state).and_then(|s| s.running);
        if running != Some(true) {
            return Ok(());
        }
        self.sync_config(&load_nginx_conf(&self.db).await?).await
    }

    async fn exec(&self, container: &str, script: &str, env: Vec<String>) -> anyhow::Result<String> {
        let exec = self
            .docker
            .create_exec(
                container,
                CreateExecOptions {
                    cmd: Some(vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()]),
                    env: Some(env),
                    // Docker unpacks uploads as root, so the file steps run as root whatever the image's user is.
                    user: Some("root".to_owned()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .with_context(|| format!("running a step in {container}"))?;
        let mut text = String::new();
        if let StartExecResults::Attached { mut output, .. } = self.docker.start_exec(&exec.id, None).await? {
            while let Some(Ok(chunk)) = output.next().await {
                text.push_str(&chunk.to_string());
            }
        }
        let code = self.docker.inspect_exec(&exec.id).await?.exit_code.unwrap_or(0);
        if code != 0 {
            bail!("{}", text.trim());
        }
        Ok(text.trim().to_owned())
    }

    async fn upload(&self, container: &str, path: &str, tar: Vec<u8>) -> anyhow::Result<()> {
        let options = UploadToContainerOptionsBuilder::new().path(path).build();
        self.docker
            .upload_to_container(container, Some(options), bollard::body_full(tar.into()))
            .await
            .with_context(|| format!("copying files to {container}:{path}"))
    }

    async fn download(&self, container: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        let options = DownloadFromContainerOptionsBuilder::new().path(path).build();
        let mut stream = self.docker.download_from_container(container, Some(options));
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.with_context(|| format!("copying {path} out of the build"))?);
        }
        Ok(out)
    }

    /// Unpacks `archive` (plain or compressed tar) as revision `revision` and makes `out` its top. Returns the file count.
    pub async fn install(&self, deployment_id: &str, revision: i64, archive: Vec<u8>, out: &str, hoist: bool) -> anyhow::Result<String> {
        let dir = format!("{deployment_id}/{revision}");
        self.exec(CONTAINER, "rm -rf -- \"$DIR\" \"$DIR.out\"", vec![format!("DIR={ROOT}/{dir}")]).await?;
        self.upload(CONTAINER, ROOT, folders_tar(&[deployment_id, &dir])?).await?;
        self.upload(CONTAINER, &format!("{ROOT}/{dir}"), archive).await?;
        let env = vec![format!("DIR={ROOT}/{dir}"), format!("OUT={out}"), format!("HOIST={}", u8::from(hoist))];
        self.exec(CONTAINER, NORMALIZE, env).await
    }

    /// Replaces a deployment's error pages. `files` are paths inside its error folder. With none, the folder goes away
    /// and a server that isn't there yet isn't started for it.
    pub async fn write_error_pages(&self, deployment_id: &str, files: &[(String, &str)]) -> anyhow::Result<()> {
        let dir = format!("{ROOT}/{ERRORS_DIR}/{deployment_id}");
        if files.is_empty() {
            if self.docker.inspect_container(CONTAINER, None).await.is_ok() {
                self.exec(CONTAINER, "rm -rf -- \"$DIR\"", vec![format!("DIR={dir}")]).await?;
            }
            return Ok(());
        }
        self.ensure().await?;
        self.exec(CONTAINER, "rm -rf -- \"$DIR\"", vec![format!("DIR={dir}")]).await?;
        let mut folders = vec![ERRORS_DIR.to_owned(), format!("{ERRORS_DIR}/{deployment_id}")];
        for (path, _) in files {
            if let Some((folder, _)) = path.rsplit_once('/') {
                let folder = format!("{ERRORS_DIR}/{deployment_id}/{folder}");
                if !folders.contains(&folder) {
                    folders.push(folder);
                }
            }
        }
        let folders: Vec<&str> = folders.iter().map(String::as_str).collect();
        self.upload(CONTAINER, ROOT, folders_tar(&folders)?).await?;
        self.upload(CONTAINER, &dir, files_tar(files)?).await
    }

    /// Removes every revision of a site except `keep`.
    pub async fn prune(&self, deployment_id: &str, keep: &[i64]) -> anyhow::Result<()> {
        let keep: Vec<String> = keep.iter().map(i64::to_string).collect();
        let script = r#"cd "$DIR" 2>/dev/null || exit 0; for r in * .[!.]*; do [ -e "$r" ] || continue; case " $KEEP " in *" $r "*) ;; *) rm -rf -- "$r" ;; esac; done"#;
        self.exec(CONTAINER, script, vec![format!("DIR={ROOT}/{deployment_id}"), format!("KEEP={}", keep.join(" "))]).await?;
        Ok(())
    }

    /// Removes a deleted site's files and its build cache.
    pub async fn remove(&self, deployment_id: &str) -> anyhow::Result<()> {
        if self.docker.inspect_container(CONTAINER, None).await.is_ok() {
            self.exec(CONTAINER, "rm -rf -- \"$DIR\"", vec![format!("DIR={ROOT}/{deployment_id}")]).await?;
        }
        if self.docker.inspect_volume(&cache_volume(deployment_id)).await.is_ok() {
            self.docker.remove_volume(&cache_volume(deployment_id), None::<bollard::query_parameters::RemoveVolumeOptions>).await?;
        }
        Ok(())
    }

    /// Copies `path` out of an image without running it. The tar's top folder is the path's last part.
    pub async fn copy_from_image(&self, image: &str, path: &str) -> anyhow::Result<Vec<u8>> {
        let labels = HashMap::from([(LABEL_MANAGED.to_owned(), "true".to_owned())]);
        let body = ContainerCreateBody { image: Some(image.into()), labels: Some(labels), ..Default::default() };
        let created = self.docker.create_container(None::<bollard::query_parameters::CreateContainerOptions>, body).await?;
        let result = self.download(&created.id, path).await;
        let _ = self.docker.remove_container(&created.id, Some(RemoveContainerOptionsBuilder::new().force(true).build())).await;
        result.with_context(|| format!("{path} is not in the built image"))
    }

    /// Runs `command` in `image` with the source in `/src`, then copies the output folder out.
    /// `source` is a tar, plain or compressed. The result's top folder is the output's last part, or `src` for the whole source.
    pub async fn build_with_command(&self, job: &CommandBuild<'_>, log: &mut (dyn FnMut(String) + Send)) -> anyhow::Result<Vec<u8>> {
        self.pull(job.image).await?;
        let volume = cache_volume(job.deployment_id);
        let labels = HashMap::from([(LABEL_MANAGED.to_owned(), "true".to_owned()), (LABEL_SITE_BUILD.to_owned(), job.deployment_id.to_owned())]);
        if self.docker.inspect_volume(&volume).await.is_err() {
            self.docker
                .create_volume(VolumeCreateRequest { name: Some(volume.clone()), labels: Some(labels.clone()), ..Default::default() })
                .await?;
        }
        let mut env = vec![
            "CI=true".to_owned(),
            "npm_config_cache=/cache/npm".to_owned(),
            "YARN_CACHE_FOLDER=/cache/yarn".to_owned(),
            "PNPM_STORE_DIR=/cache/pnpm".to_owned(),
            "BUN_INSTALL_CACHE_DIR=/cache/bun".to_owned(),
        ];
        env.extend(job.env.iter().cloned());
        let body = ContainerCreateBody {
            image: Some(job.image.into()),
            cmd: Some(vec!["/bin/sh".to_owned(), "-c".to_owned(), job.command.to_owned()]),
            working_dir: Some("/src".into()),
            env: Some(env),
            labels: Some(labels),
            host_config: Some(HostConfig { binds: Some(vec![format!("{volume}:/cache")]), ..Default::default() }),
            ..Default::default()
        };
        let created = self.docker.create_container(None::<bollard::query_parameters::CreateContainerOptions>, body).await.context("creating the build container")?;
        let id = created.id;
        let result = self.run_build(&id, job, log).await;
        let _ = self.docker.remove_container(&id, Some(RemoveContainerOptionsBuilder::new().force(true).build())).await;
        result
    }

    async fn run_build(&self, id: &str, job: &CommandBuild<'_>, log: &mut (dyn FnMut(String) + Send)) -> anyhow::Result<Vec<u8>> {
        self.upload(id, "/", folders_tar(&["src"])?).await?;
        self.upload(id, "/src", job.source.to_vec()).await?;
        self.docker.start_container(id, None).await.context("starting the build")?;
        log(format!("running {} in {}", job.command, job.image));
        let follow = async {
            let options = LogsOptionsBuilder::new().follow(true).stdout(true).stderr(true).build();
            let mut logs = self.docker.logs(id, Some(options));
            while let Some(Ok(chunk)) = logs.next().await {
                for line in chunk.to_string().lines().filter(|l| !l.trim().is_empty()) {
                    log(line.to_owned());
                }
            }
        };
        if tokio::time::timeout(BUILD_TIMEOUT, follow).await.is_err() {
            bail!("the build took longer than 30 minutes");
        }
        let code = self.docker.inspect_container(id, None).await?.state.and_then(|s| s.exit_code).unwrap_or(-1);
        if code != 0 {
            bail!("the build command exited with {code}");
        }
        let path = if job.output.is_empty() { "/src".to_owned() } else { format!("/src/{}", job.output) };
        self.download(id, &path).await.with_context(|| format!("the build has no {} folder", job.output))
    }
}

pub struct CommandBuild<'a> {
    pub deployment_id: &'a str,
    pub image: &'a str,
    pub command: &'a str,
    /// Relative to the source, empty for the whole source.
    pub output: &'a str,
    pub env: &'a [String],
    pub source: &'a [u8],
}

/// The last part of a path, which is the top folder of a tar that Docker copies out of a container.
pub fn top_folder(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("src").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, tls: bool) -> SiteRoute {
        SiteRoute { hostname: name.into(), tls, cert: RouteCert::Acme { wildcard_zone: None }, redirect: None }
    }

    #[test]
    fn redirect_hosts_of_a_site_skip_the_files() {
        let old = SiteRoute { redirect: Some("https://docs.example.com".into()), ..host("old-docs.example.com", true) };
        let out = routes("docs", "d1", Some(4), false, &[host("docs.example.com", true), old]);
        let routers = &out["http"]["routers"];
        assert_eq!(routers["docs-old-docs-example-com"]["service"], crate::proxy::routes::NOOP_SERVICE);
        assert_eq!(routers["docs-old-docs-example-com-http"]["middlewares"], json!(["docs-old-docs-example-com-redirect"]));
        assert_eq!(routers["docs-docs-example-com"]["service"], SERVICE);
        assert!(out["http"]["middlewares"]["docs-old-docs-example-com-redirect"]["redirectRegex"].is_object());
    }

    #[test]
    fn file_types_can_not_break_out_of_the_types_block() {
        let ok = parse_mime_types(r#"{"md":"text/markdown","webmanifest":"application/manifest+json","glb":"model/gltf-binary"}"#).unwrap();
        assert_eq!(ok.len(), 3);
        for bad in [
            r#"{"js":"text/plain; } server { listen 81; root /; } types {"}"#,
            r#"{"js; } location / { return 200; } types { a/b":"text/plain"}"#,
            r#"{"md":"text/plain;charset=utf-8"}"#,
            r#"{".mjs":"application/javascript"}"#,
            r#"{"md":"markdown"}"#,
            r#"{"md":"text/plain\n"}"#,
        ] {
            assert!(parse_mime_types(bad).is_err(), "{bad}");
        }
        assert!(parse_mime_types("").unwrap().is_empty(), "a server that never saved the setting");
    }

    fn types(pairs: &[(&str, &str)]) -> MimeTypes {
        pairs.iter().map(|(e, t)| ((*e).to_owned(), (*t).to_owned())).collect()
    }

    fn plain_conf() -> String {
        nginx_conf(&MimeTypes::new(), &BTreeMap::new()).unwrap()
    }

    #[test]
    fn saved_file_types_replace_the_defaults() {
        let conf = nginx_conf(&types(&[("mjs", "text/javascript")]), &BTreeMap::new()).unwrap();
        assert!(conf.contains("text/javascript mjs;"));
        assert!(!conf.contains("application/javascript mjs;"), "nginx would warn about a duplicate extension");
    }

    #[test]
    fn a_site_layers_its_types_over_the_global_ones() {
        let id = "5f0c2a8e-1b7d-4c3e-9a61-2d4b8e7f0a13";
        let global = types(&[("md", "text/markdown"), ("glb", "model/gltf-binary")]);
        let sites = BTreeMap::from([(id.to_owned(), types(&[("md", "text/plain")]))]);
        let conf = nginx_conf(&global, &sites).unwrap();
        let site = conf.find(&format!("location ~ ^/(?<site>{id})/")).expect("the site has its own location");
        let generic = conf.find("location ~ ^/(?<site>[^/]+)/").unwrap();
        assert!(site < generic, "nginx uses the first regex location that matches");
        let block = &conf[site..generic];
        assert!(block.contains("include /etc/nginx/mime.types;"), "html and css would lose their types otherwise");
        assert!(block.contains("text/plain md;") && !block.contains("text/markdown md;"));
        assert!(block.contains("model/gltf-binary glb;") && block.contains("application/javascript mjs;"));
        assert!(block.contains("$kuroe_fallback"), "single-page apps keep their fallback");
        assert!(conf[..site].contains("text/markdown md;"), "other sites keep the global type");
    }

    #[test]
    fn error_pages_are_served_as_plain_files() {
        let conf = plain_conf();
        assert!(conf.contains("location ^~ /_errors/"), "error pages skip the per-site rules");
        assert!(conf.contains("add_header Retry-After 120 always;"));
        assert!(conf.contains("add_header Cache-Control \"no-store\" always;"));
    }

    #[test]
    fn routes_point_at_the_active_revision() {
        let out = routes("docs", "d1", Some(4), false, &[host("docs.example.com", true), host("plain.test", false)]);
        let routers = &out["http"]["routers"];
        assert_eq!(routers["docs-docs-example-com"]["service"], SERVICE);
        assert_eq!(routers["docs-docs-example-com"]["middlewares"], json!(["docs-site", "docs-spa"]));
        assert_eq!(routers["docs-docs-example-com"]["tls"], json!({ "certResolver": "le" }));
        assert_eq!(routers["docs-docs-example-com-http"]["middlewares"], json!([HTTPS_MIDDLEWARE]));
        assert_eq!(routers["docs-plain-test"]["entryPoints"], json!(["web"]));
        assert_eq!(out["http"]["middlewares"]["docs-site"], json!({ "addPrefix": { "prefix": "/d1/4" } }));
        assert_eq!(
            out["http"]["middlewares"]["docs-spa"],
            json!({ "headers": { "customRequestHeaders": { "X-Kuroe-Spa": "" } } }),
            "a client can't turn the fallback on"
        );
        assert!(out["http"].get("services").is_none(), "the shared service lives in _kuroe.yml");
    }

    #[test]
    fn spa_sites_ask_nginx_for_the_fallback() {
        let out = routes("app", "d2", Some(7), true, &[host("app.test", false)]);
        assert_eq!(out["http"]["routers"]["app-app-test"]["middlewares"], json!(["app-site", "app-spa"]));
        assert_eq!(out["http"]["middlewares"]["app-spa"], json!({ "headers": { "customRequestHeaders": { "X-Kuroe-Spa": "1" } } }));
        assert!(plain_conf().contains("map $http_x_kuroe_spa $kuroe_fallback"), "nginx reads the header Traefik sets");
    }

    #[test]
    fn nothing_routed_when_stopped_or_without_domains() {
        assert_eq!(routes("x", "d", None, false, &[host("x.test", false)]), json!({}));
        assert_eq!(routes("x", "d", Some(1), false, &[]), json!({}));
    }

    fn tar_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, body) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, body.as_bytes()).unwrap();
        }
        builder.into_inner().unwrap()
    }

    /// Needs Docker and pulls nginx and busybox. Run with `cargo test -- --ignored sites::`.
    #[tokio::test]
    #[ignore = "needs a reachable Docker daemon"]
    async fn installs_builds_and_serves_through_the_shared_server() {
        let db = crate::db::test_pool().await;
        sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?)")
            .bind(MIME_TYPES_SETTING)
            .bind(r#"{"md":"text/plain"}"#)
            .execute(&db)
            .await
            .unwrap();
        let sites = Sites { docker: crate::docker::connect().unwrap(), db };
        sites.ensure().await.unwrap();
        sites.ensure().await.unwrap();
        let id = format!("test-{}", uuid::Uuid::new_v4());
        let get = |rev: i64, path: &str, spa: bool| {
            let sites = sites.clone();
            let url = format!("http://127.0.0.1/{id}/{rev}/{path}");
            let header = if spa { "--header X-Kuroe-Spa:1" } else { "" };
            async move {
                sites.exec(CONTAINER, &format!("wget -qO- {header} \"$URL\""), vec![format!("URL={url}")]).await
            }
        };
        let fetch = |rev: i64, path: &str| get(rev, path, false);

        let archive = tar_of(&[
            ("dist/index.html", "from an archive"),
            ("dist/assets/app.js", "js"),
            ("dist/assets/chunk.mjs", "export {}"),
            ("dist/notes.md", "# notes"),
            ("dist/docs/index.html", "docs page"),
            ("dist/.env", "SECRET=1"),
        ]);
        let count = sites.install(&id, 1, archive, "", true).await.unwrap();
        assert_eq!(count, "6", "a lone dist folder is unpacked from inside");
        let content_type = |path: &str| {
            let sites = sites.clone();
            let url = format!("http://127.0.0.1/{id}/1/{path}");
            async move { sites.exec(CONTAINER, "wget -S --spider \"$URL\" 2>&1", vec![format!("URL={url}")]).await.unwrap() }
        };
        assert!(content_type("assets/chunk.mjs").await.contains("Content-Type: application/javascript"), "modules load as scripts");
        assert!(content_type("notes.md").await.contains("Content-Type: text/plain"), "types from the setting apply");

        let owner = crate::auth::create_user(&sites.db, "boss", "boss-password-1", crate::auth::Role::owner()).await.unwrap();
        let mut spec = crate::deploy::Spec::new(crate::deploy::Source::Static {});
        spec.site = Some(crate::deploy::spec::SiteOptions { mime_types: types(&[("md", "text/markdown")]), ..Default::default() });
        let own = crate::deploy::create(&sites.db, "notes", &spec, &owner).await.unwrap();
        sites.refresh_config().await.unwrap();
        sites.install(&own.id, 1, tar_of(&[("notes.md", "# own"), ("index.html", "home")]), "", false).await.unwrap();
        let own_type = |path: &str| {
            let sites = sites.clone();
            let url = format!("http://127.0.0.1/{}/1/{path}", own.id);
            async move { sites.exec(CONTAINER, "wget -S --spider \"$URL\" 2>&1", vec![format!("URL={url}")]).await.unwrap() }
        };
        assert!(own_type("notes.md").await.contains("Content-Type: text/markdown"), "a site's own type wins");
        assert!(own_type("index.html").await.contains("Content-Type: text/html"), "the stock types still apply");
        assert!(content_type("notes.md").await.contains("Content-Type: text/plain"), "other sites keep the global type");
        sites.remove(&own.id).await.unwrap();
        assert_eq!(fetch(1, "index.html").await.unwrap(), "from an archive");
        assert_eq!(fetch(1, "").await.unwrap(), "from an archive", "a folder answers with its index.html");
        assert_eq!(fetch(1, "docs/").await.unwrap(), "docs page");
        assert!(fetch(1, ".env").await.is_err(), "hidden files are never served");
        assert!(fetch(1, "settings/profile").await.is_err(), "no fallback without the header");
        assert_eq!(get(1, "settings/profile", true).await.unwrap(), "from an archive", "single-page apps fall back to index.html");

        let source = tar_of(&[("package.json", "{}")]);
        let job = CommandBuild {
            deployment_id: &id,
            image: "busybox:1.37",
            command: "mkdir -p out && echo \"built with $GREETING\" > out/index.html",
            output: "out",
            env: &["GREETING=env".to_owned()],
            source: &source,
        };
        let mut lines = Vec::new();
        let built = sites.build_with_command(&job, &mut |l| lines.push(l)).await.unwrap();
        sites.install(&id, 2, built, "out", false).await.unwrap();
        assert_eq!(fetch(2, "index.html").await.unwrap(), "built with env");

        let failing = CommandBuild { command: "echo broken >&2; exit 4", ..job };
        let err = sites.build_with_command(&failing, &mut |_| {}).await.unwrap_err();
        assert!(err.to_string().contains("exited with 4"), "{err:#}");

        let page = |path: &str| {
            let sites = sites.clone();
            let url = format!("http://127.0.0.1/_errors/{id}/{path}");
            async move { sites.exec(CONTAINER, "wget -qO- \"$URL\"", vec![format!("URL={url}")]).await }
        };
        sites.write_error_pages(&id, &[("dm1/default.html".into(), "back soon"), ("dm1/0.json".into(), "{\"error\":\"down\"}")]).await.unwrap();
        assert_eq!(page("dm1/default.html").await.unwrap(), "back soon");
        assert_eq!(page("dm1/0.json").await.unwrap(), "{\"error\":\"down\"}");
        sites.write_error_pages(&id, &[("dm1/default.html".into(), "changed")]).await.unwrap();
        assert_eq!(page("dm1/default.html").await.unwrap(), "changed");
        assert!(page("dm1/0.json").await.is_err(), "pages that were removed are gone");
        sites.write_error_pages(&id, &[]).await.unwrap();
        assert!(page("dm1/default.html").await.is_err());

        sites.prune(&id, &[2]).await.unwrap();
        assert!(fetch(1, "index.html").await.is_err(), "revision 1 was pruned");
        sites.remove(&id).await.unwrap();
        assert!(fetch(2, "index.html").await.is_err());
    }

    #[test]
    fn archive_kinds_and_projects() {
        let mut tar = vec![0u8; 512];
        tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(archive_kind(&tar), Some(ArchiveKind::Tar));
        assert_eq!(archive_kind(&[0x1f, 0x8b, 8]), Some(ArchiveKind::Gzip));
        assert_eq!(archive_kind(b"BZh91AY"), Some(ArchiveKind::Bzip2));
        assert_eq!(archive_kind(&[0xfd, b'7', b'z', b'X', b'Z', 0, 1]), Some(ArchiveKind::Xz));
        assert_eq!(archive_kind(b"PK\x03\x04 zip"), None);
        assert_eq!(revision_of(&project(12)), Some(12));
        assert_eq!(revision_of("kuroe_shop-blue"), None);
        assert_eq!(top_folder("/app/dist/"), "dist");
        assert_eq!(top_folder("/src"), "src");
    }
}
