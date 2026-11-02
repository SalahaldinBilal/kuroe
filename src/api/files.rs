use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::query_parameters::{DownloadFromContainerOptionsBuilder, UploadToContainerOptionsBuilder};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use super::error::{ApiError, ApiResult};
use super::extract::CurrentUser;
use super::{AppState, access};
use crate::auth::Permission;

const LIST_TIMEOUT: Duration = Duration::from_secs(10);
const SIZES_TIMEOUT: Duration = Duration::from_secs(15);
const SIZES_TTL: Duration = Duration::from_secs(60);
pub const MEDIA_TTL: Duration = Duration::from_secs(10 * 60);
const MEDIA_LIMIT: u64 = 2 << 30;
const SNIFF_BYTES: usize = 8 << 10;
const CHUNK: usize = 64 << 10;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Lister {
    Exec,
    Archive,
}

#[derive(Default)]
pub struct FileState {
    listers: Mutex<HashMap<String, Lister>>,
    sizes: Mutex<HashMap<(String, String), (Instant, Sizes)>>,
    media: Mutex<HashMap<String, Instant>>,
    media_lock: tokio::sync::Mutex<()>,
}

fn docker_error(err: bollard::errors::Error) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, format!("docker: {err}"))
}

#[derive(Deserialize)]
pub struct PathQuery {
    path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry {
    pub name: String,
    pub kind: &'static str,
    pub size: u64,
    pub mode: u32,
    pub modified: u64,
    pub link: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub link_dir: bool,
}

fn sort_entries(entries: &mut [Entry]) {
    let folderish = |e: &Entry| e.kind == "dir" || e.link_dir;
    entries.sort_by(|a, b| (!folderish(a), &a.name).cmp(&(!folderish(b), &b.name)));
}

// One stat for every entry, name last so spaces and tabs in names survive. Links get a second line with their target.
const LIST_SCRIPT: &str = r#"command -v stat >/dev/null 2>&1 || exit 4
cd -- "$1" 2>/dev/null || exit 3
shift
for f in * .[!.]* ..?*; do
  [ -e "$f" ] || [ -L "$f" ] || continue
  set -- "$@" "$f"
done
[ $# -eq 0 ] && exit 0
stat -c '%F	%s	%a	%Y	%n' -- "$@" 2>/dev/null
for f; do
  if [ -L "$f" ]; then
    if [ -d "$f" ]; then t=LD; else t=LF; fi
    printf '%s\t%s\t%s\n' "$t" "$(readlink -- "$f")" "$f"
  fi
done
exit 0"#;

pub fn parse_stat(output: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut links = Vec::new();
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("LD\t").map(|r| (r, true)).or_else(|| line.strip_prefix("LF\t").map(|r| (r, false))) {
            let (rest, is_dir) = rest;
            if let Some((target, name)) = rest.split_once('\t') {
                links.push((name.to_owned(), target.to_owned(), is_dir));
            }
            continue;
        }
        let mut parts = line.splitn(5, '\t');
        let (Some(kind), Some(size), Some(mode), Some(modified), Some(name)) =
            (parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let kind = match kind {
            "directory" => "dir",
            "symbolic link" => "link",
            "regular file" | "regular empty file" => "file",
            _ => "other",
        };
        entries.push(Entry {
            name: name.to_owned(),
            kind,
            size: size.parse().unwrap_or(0),
            mode: u32::from_str_radix(mode, 8).unwrap_or(0),
            modified: modified.parse().unwrap_or(0),
            link: None,
            link_dir: false,
        });
    }
    for (name, target, is_dir) in links {
        if let Some(entry) = entries.iter_mut().find(|e| e.name == name) {
            entry.link = Some(target);
            entry.link_dir = is_dir;
        }
    }
    sort_entries(&mut entries);
    entries
}

#[derive(Debug)]
struct ExecRun {
    stdout: String,
    exit: Option<i64>,
    timed_out: bool,
}

#[derive(Debug, PartialEq)]
enum ExecFailure {
    Stopped,
    NoShell,
    NoStat,
    Other(String),
}

async fn run_exec(docker: &Docker, id: &str, cmd: Vec<String>, timeout: Duration) -> Result<ExecRun, ExecFailure> {
    let exec = docker
        .create_exec(
            id,
            CreateExecOptions {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(cmd),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| match e {
            bollard::errors::Error::DockerResponseServerError { status_code: 409, .. } => ExecFailure::Stopped,
            other => ExecFailure::Other(other.to_string()),
        })?;
    let started = docker
        .start_exec(&exec.id, Some(StartExecOptions { detach: false, tty: false, output_capacity: None }))
        .await
        .map_err(|_| ExecFailure::NoShell)?;
    let StartExecResults::Attached { mut output, .. } = started else {
        return Err(ExecFailure::Other("exec started detached".into()));
    };
    let mut stdout = Vec::new();
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut timed_out = false;
    loop {
        tokio::select! {
            chunk = output.next() => match chunk {
                Some(Ok(LogOutput::StdOut { message })) => stdout.extend_from_slice(&message),
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
            _ = &mut deadline => { timed_out = true; break; }
        }
    }
    let exit = if timed_out { None } else { exit_code(docker, &exec.id).await };
    if exit == Some(127) {
        return Err(ExecFailure::NoShell);
    }
    Ok(ExecRun { stdout: String::from_utf8_lossy(&stdout).into_owned(), exit, timed_out })
}

/// The output can end a moment before Docker records the exit code.
async fn exit_code(docker: &Docker, exec_id: &str) -> Option<i64> {
    for _ in 0..20 {
        let inspect = docker.inspect_exec(exec_id).await.ok()?;
        if inspect.running != Some(true) {
            return inspect.exit_code;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

fn shell(script: &str, args: &[&str]) -> Vec<String> {
    let mut cmd = vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned(), "sh".to_owned()];
    cmd.extend(args.iter().map(|a| (*a).to_owned()));
    cmd
}

/// Turns the archive stream into a blocking reader, so the tar parser can walk it in a worker thread without
/// buffering the whole archive.
struct ChannelReader {
    rx: mpsc::Receiver<std::io::Result<Bytes>>,
    buf: Bytes,
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.buf.is_empty() {
            match self.rx.blocking_recv() {
                Some(Ok(chunk)) => self.buf = chunk,
                Some(Err(err)) => return Err(err),
                None => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len());
        out[..n].copy_from_slice(&self.buf[..n]);
        self.buf = self.buf.slice(n..);
        Ok(n)
    }
}

async fn archive(docker: &Docker, id: &str, path: &str) -> ApiResult<ChannelReader> {
    let options = DownloadFromContainerOptionsBuilder::new().path(path).build();
    let mut stream = docker.download_from_container(id, Some(options));
    let first = match stream.next().await {
        Some(Ok(chunk)) => Some(chunk),
        Some(Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. })) => {
            return Err(ApiError::new(StatusCode::NOT_FOUND, format!("{path} does not exist")));
        }
        Some(Err(err)) => return Err(docker_error(err)),
        None => None,
    };
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(async move {
        if let Some(first) = first
            && tx.send(Ok(first)).await.is_err()
        {
            return;
        }
        while let Some(chunk) = stream.next().await {
            if tx.send(chunk.map_err(std::io::Error::other)).await.is_err() {
                return;
            }
        }
    });
    Ok(ChannelReader { rx, buf: Bytes::new() })
}

/// Keeps only the direct children. File bodies are read past, never stored.
pub fn list_tar(reader: impl Read) -> std::io::Result<Vec<Entry>> {
    let mut archive = tar::Archive::new(reader);
    let mut entries = Vec::new();
    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?.into_owned();
        let parts: Vec<_> = path.components().collect();
        let header = entry.header();
        let kind = match header.entry_type() {
            tar::EntryType::Directory => "dir",
            tar::EntryType::Symlink => "link",
            t if t.is_file() => "file",
            _ => "other",
        };
        let entry = Entry {
            name: parts.last().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default(),
            kind,
            size: header.size().unwrap_or(0),
            mode: header.mode().unwrap_or(0),
            modified: header.mtime().unwrap_or(0),
            link: header.link_name().ok().flatten().map(|l| l.to_string_lossy().into_owned()),
            link_dir: false,
        };
        match parts.len() {
            1 if kind != "dir" => return Ok(vec![entry]),
            2 => entries.push(entry),
            _ => {}
        }
    }
    sort_entries(&mut entries);
    Ok(entries)
}

async fn list_by_archive(state: &AppState, id: &str, path: &str) -> ApiResult<Vec<Entry>> {
    let reader = archive(&state.docker, id, path).await?;
    tokio::task::spawn_blocking(move || list_tar(reader))
        .await
        .map_err(anyhow::Error::from)?
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("reading archive: {e}")))
}

#[derive(Serialize)]
pub struct Listing {
    entries: Vec<Entry>,
    note: Option<&'static str>,
}

const NO_SHELL_NOTE: &str = "This image has no shell, large folders load slowly.";
const STOPPED_NOTE: &str = "The container is stopped, large folders load slowly.";

pub async fn list(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> ApiResult<Json<Listing>> {
    access::container(&state, &current, &id, Permission::DepShell).await?;
    let known = state.files.listers.lock().unwrap().get(&id).copied();
    if known != Some(Lister::Archive) {
        let run = run_exec(&state.docker, &id, shell(LIST_SCRIPT, &[&query.path]), LIST_TIMEOUT)
            .await
            .and_then(|run| if run.exit == Some(4) { Err(ExecFailure::NoStat) } else { Ok(run) });
        match run {
            Ok(run) if run.exit == Some(0) => {
                state.files.listers.lock().unwrap().insert(id.clone(), Lister::Exec);
                return Ok(Json(Listing { entries: parse_stat(&run.stdout), note: None }));
            }
            Ok(run) if run.exit == Some(3) => {
                return Err(ApiError::new(StatusCode::NOT_FOUND, format!("{} is not a folder", query.path)));
            }
            Ok(_) => {}
            Err(ExecFailure::NoShell) | Err(ExecFailure::NoStat) => {
                state.files.listers.lock().unwrap().insert(id.clone(), Lister::Archive);
            }
            Err(ExecFailure::Stopped) => {
                let entries = list_by_archive(&state, &id, &query.path).await?;
                return Ok(Json(Listing { entries, note: Some(STOPPED_NOTE) }));
            }
            Err(ExecFailure::Other(_)) => {}
        }
    }
    let entries = list_by_archive(&state, &id, &query.path).await?;
    let note = (state.files.listers.lock().unwrap().get(&id) == Some(&Lister::Archive)).then_some(NO_SHELL_NOTE);
    Ok(Json(Listing { entries, note }))
}

const SIZES_SCRIPT: &str = r#"cd -- "$1" 2>/dev/null || exit 3
shift
for f in * .[!.]* ..?*; do
  [ -d "$f" ] && [ ! -L "$f" ] || continue
  case "${PWD%/}/$f" in /proc|/sys|/dev) continue ;; esac
  set -- "$@" "$f"
done
[ $# -eq 0 ] && exit 0
if timeout 15 true 2>/dev/null; then exec timeout 15 du -skx -- "$@"; fi
exec du -skx -- "$@""#;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Sizes {
    sizes: BTreeMap<String, u64>,
    complete: bool,
}

pub fn parse_du(output: &str) -> BTreeMap<String, u64> {
    output
        .lines()
        .filter_map(|line| {
            let (kib, name) = line.split_once('\t')?;
            Some((name.trim_end_matches('/').to_owned(), kib.trim().parse::<u64>().ok()? * 1024))
        })
        .collect()
}

pub async fn sizes(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> ApiResult<Json<Sizes>> {
    access::container(&state, &current, &id, Permission::DepShell).await?;
    let key = (id.clone(), query.path.clone());
    if let Some((at, cached)) = state.files.sizes.lock().unwrap().get(&key)
        && at.elapsed() < SIZES_TTL
    {
        return Ok(Json(cached.clone()));
    }
    let run = match run_exec(&state.docker, &id, shell(SIZES_SCRIPT, &[&query.path]), SIZES_TIMEOUT).await {
        Ok(run) => run,
        Err(_) => return Ok(Json(Sizes { sizes: BTreeMap::new(), complete: false })),
    };
    let result = Sizes { sizes: parse_du(&run.stdout), complete: !run.timed_out && run.exit == Some(0) };
    state.files.sizes.lock().unwrap().insert(key, (Instant::now(), result.clone()));
    Ok(Json(result))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Meta {
    kind: &'static str,
    size: u64,
    mode: u32,
    modified: u64,
}

type FileBody = mpsc::Receiver<std::io::Result<Bytes>>;

/// Reads the archive's first entry. For a regular file its body is sent on, up to `max` bytes.
fn first_entry(reader: ChannelReader, max: u64, meta: oneshot::Sender<std::io::Result<Meta>>, body: mpsc::Sender<std::io::Result<Bytes>>) {
    let mut archive = tar::Archive::new(reader);
    let mut entries = match archive.entries() {
        Ok(entries) => entries,
        Err(err) => {
            let _ = meta.send(Err(err));
            return;
        }
    };
    let mut entry = match entries.next() {
        Some(Ok(entry)) => entry,
        Some(Err(err)) => {
            let _ = meta.send(Err(err));
            return;
        }
        None => {
            let _ = meta.send(Err(std::io::Error::other("the archive is empty")));
            return;
        }
    };
    let header = entry.header();
    let kind = match header.entry_type() {
        tar::EntryType::Directory => "dir",
        t if t.is_file() => "file",
        _ => "other",
    };
    let info = Meta { kind, size: header.size().unwrap_or(0), mode: header.mode().unwrap_or(0), modified: header.mtime().unwrap_or(0) };
    if meta.send(Ok(info)).is_err() || kind != "file" {
        return;
    }
    let mut left = max;
    let mut buf = vec![0u8; CHUNK];
    while left > 0 {
        let want = buf.len().min(left as usize);
        match entry.read(&mut buf[..want]) {
            Ok(0) => return,
            Ok(n) => {
                left -= n as u64;
                if body.blocking_send(Ok(Bytes::copy_from_slice(&buf[..n]))).is_err() {
                    return;
                }
            }
            Err(err) => {
                let _ = body.blocking_send(Err(err));
                return;
            }
        }
    }
}

async fn open_file(state: &AppState, id: &str, path: &str, max: u64) -> ApiResult<(Meta, FileBody)> {
    let reader = archive(&state.docker, id, path).await?;
    let (meta_tx, meta_rx) = oneshot::channel();
    let (body_tx, body_rx) = mpsc::channel(4);
    tokio::task::spawn_blocking(move || first_entry(reader, max, meta_tx, body_tx));
    let meta = meta_rx
        .await
        .map_err(anyhow::Error::from)?
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("reading archive: {e}")))?;
    Ok((meta, body_rx))
}

fn body_stream(rx: FileBody) -> Body {
    Body::from_stream(futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|item| (item, rx)) }))
}

fn file_name(path: &str) -> String {
    path.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("root").to_owned()
}

fn attachment(name: &str) -> String {
    let safe: String = name.chars().map(|c| if c == '"' || c == '\\' || c.is_control() { '_' } else { c }).collect();
    format!("attachment; filename=\"{safe}\"")
}

pub async fn download(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> ApiResult<Response> {
    access::container(&state, &current, &id, Permission::DepShell).await?;
    let name = file_name(&query.path);
    let (meta, body) = open_file(&state, &id, &query.path, u64::MAX).await?;
    if meta.kind == "file" {
        let mut response = body_stream(body).into_response();
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(meta.size));
        headers.insert(header::CONTENT_DISPOSITION, header_value(&attachment(&name)));
        return Ok(response);
    }
    drop(body);
    let options = DownloadFromContainerOptionsBuilder::new().path(&query.path).build();
    let raw = state.docker.download_from_container(&id, Some(options)).map(|chunk| chunk.map_err(std::io::Error::other));
    let mut response = Body::from_stream(raw).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-tar"));
    headers.insert(header::CONTENT_DISPOSITION, header_value(&attachment(&format!("{name}.tar"))));
    Ok(response)
}

fn header_value(text: &str) -> HeaderValue {
    HeaderValue::from_str(text).unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Preview {
    Text,
    Image,
    Svg,
    Video,
    Audio,
    Pdf,
    Binary,
}

pub fn looks_like_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

/// HTML and other markup come back as text, so the browser never renders them.
pub fn preview_kind(name: &str, head: &[u8]) -> (Preview, String) {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let guessed = || mime_guess::from_ext(&ext).first_or_octet_stream().essence_str().to_owned();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "bmp" | "ico" => (Preview::Image, guessed()),
        "svg" => (Preview::Svg, "image/svg+xml".into()),
        "mp4" | "webm" | "mov" => (Preview::Video, guessed()),
        "mp3" | "ogg" | "wav" | "flac" => (Preview::Audio, guessed()),
        "pdf" => (Preview::Pdf, "application/pdf".into()),
        _ if looks_like_text(head) => (Preview::Text, "text/plain; charset=utf-8".into()),
        _ => (Preview::Binary, "application/octet-stream".into()),
    }
}

fn preview_headers(headers: &mut HeaderMap, content_type: &str, kind: Preview) {
    headers.insert(header::CONTENT_TYPE, header_value(content_type));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("inline"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    if kind != Preview::Pdf {
        headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox"));
    }
}

#[derive(Deserialize)]
pub struct PreviewQuery {
    path: String,
    #[serde(default)]
    meta: bool,
    max: Option<u64>,
}

#[derive(Serialize)]
pub struct PreviewMeta {
    #[serde(flatten)]
    meta: Meta,
    preview: Option<Preview>,
}

pub async fn preview(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(query): Query<PreviewQuery>,
) -> ApiResult<Response> {
    access::container(&state, &current, &id, Permission::DepShell).await?;
    let max = if query.meta { SNIFF_BYTES as u64 } else { query.max.unwrap_or(u64::MAX) };
    let (meta, mut body) = open_file(&state, &id, &query.path, max).await?;
    if meta.kind != "file" {
        if query.meta {
            return Ok(Json(PreviewMeta { meta, preview: None }).into_response());
        }
        return Err(ApiError::bad_request("only files have a preview"));
    }
    let mut head = Vec::new();
    while head.len() < SNIFF_BYTES {
        match body.recv().await {
            Some(Ok(chunk)) => head.extend_from_slice(&chunk),
            Some(Err(err)) => return Err(ApiError::new(StatusCode::BAD_GATEWAY, format!("reading the file: {err}"))),
            None => break,
        }
    }
    let (kind, content_type) = preview_kind(&file_name(&query.path), &head[..head.len().min(SNIFF_BYTES)]);
    if query.meta {
        return Ok(Json(PreviewMeta { meta, preview: Some(kind) }).into_response());
    }
    let length = meta.size.min(max);
    let rest = futures::stream::unfold(body, |mut rx| async move { rx.recv().await.map(|item| (item, rx)) });
    let stream = futures::stream::once(async move { Ok::<_, std::io::Error>(Bytes::from(head)) }).chain(rest);
    let mut response = Body::from_stream(stream).into_response();
    let headers = response.headers_mut();
    preview_headers(headers, &content_type, kind);
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    headers.insert("x-kuroe-size", HeaderValue::from(meta.size));
    Ok(response)
}

fn media_dir(state: &AppState) -> PathBuf {
    state.runner.data_dir.join("tmp").join("media")
}

fn media_key(container: &str, path: &str) -> String {
    let digest = Sha256::digest(format!("{container}\0{path}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

async fn ensure_media(state: &AppState, id: &str, path: &str) -> ApiResult<PathBuf> {
    let key = media_key(id, path);
    let file = media_dir(state).join(&key);
    let _guard = state.files.media_lock.lock().await;
    if state.files.media.lock().unwrap().contains_key(&key) && tokio::fs::try_exists(&file).await.unwrap_or(false) {
        return Ok(file);
    }
    let (meta, mut body) = open_file(state, id, path, MEDIA_LIMIT + 1).await?;
    if meta.kind != "file" {
        return Err(ApiError::bad_request("only files can be played"));
    }
    if meta.size > MEDIA_LIMIT {
        return Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "files over 2 GiB can't be played here, download it instead"));
    }
    tokio::fs::create_dir_all(media_dir(state)).await.map_err(anyhow::Error::from)?;
    let partial = file.with_extension("part");
    let mut out = tokio::fs::File::create(&partial).await.map_err(anyhow::Error::from)?;
    while let Some(chunk) = body.recv().await {
        let chunk = chunk.map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("reading the file: {e}")))?;
        out.write_all(&chunk).await.map_err(anyhow::Error::from)?;
    }
    out.flush().await.map_err(anyhow::Error::from)?;
    tokio::fs::rename(&partial, &file).await.map_err(anyhow::Error::from)?;
    state.files.media.lock().unwrap().insert(key, Instant::now());
    Ok(file)
}

#[derive(Debug, PartialEq)]
pub enum RangeError {
    Unsatisfiable,
}

/// One `bytes=` range. Several ranges in one header aren't supported and are served whole.
pub fn parse_range(value: Option<&str>, size: u64) -> Result<Option<(u64, u64)>, RangeError> {
    let Some(spec) = value.and_then(|v| v.trim().strip_prefix("bytes=")) else { return Ok(None) };
    if spec.contains(',') {
        return Ok(None);
    }
    let Some((start, end)) = spec.split_once('-') else { return Ok(None) };
    let range = match (start.trim(), end.trim()) {
        ("", "") => return Ok(None),
        ("", suffix) => {
            let n: u64 = suffix.parse().map_err(|_| RangeError::Unsatisfiable)?;
            if n == 0 || size == 0 {
                return Err(RangeError::Unsatisfiable);
            }
            (size.saturating_sub(n), size - 1)
        }
        (start, end) => {
            let start: u64 = start.parse().map_err(|_| RangeError::Unsatisfiable)?;
            let end = if end.is_empty() { size.saturating_sub(1) } else { end.parse::<u64>().map_err(|_| RangeError::Unsatisfiable)?.min(size.saturating_sub(1)) };
            if start >= size || end < start {
                return Err(RangeError::Unsatisfiable);
            }
            (start, end)
        }
    };
    Ok(Some(range))
}

pub async fn ranged_file(file: &std::path::Path, content_type: &str, range: Option<&str>) -> ApiResult<Response> {
    let mut handle = tokio::fs::File::open(file).await.map_err(anyhow::Error::from)?;
    let size = handle.metadata().await.map_err(anyhow::Error::from)?.len();
    let (status, start, end) = match parse_range(range, size) {
        Ok(Some((start, end))) => (StatusCode::PARTIAL_CONTENT, start, end),
        Ok(None) => (StatusCode::OK, 0, size.saturating_sub(1)),
        Err(RangeError::Unsatisfiable) => {
            let mut response = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            response.headers_mut().insert(header::CONTENT_RANGE, header_value(&format!("bytes */{size}")));
            return Ok(response);
        }
    };
    let length = if size == 0 { 0 } else { end - start + 1 };
    handle.seek(std::io::SeekFrom::Start(start)).await.map_err(anyhow::Error::from)?;
    let reader = tokio::io::AsyncReadExt::take(handle, length);
    let mut response = Body::from_stream(tokio_util::io::ReaderStream::new(reader)).into_response();
    *response.status_mut() = status;
    let headers = response.headers_mut();
    preview_headers(headers, content_type, Preview::Video);
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    if status == StatusCode::PARTIAL_CONTENT {
        headers.insert(header::CONTENT_RANGE, header_value(&format!("bytes {start}-{end}/{size}")));
    }
    Ok(response)
}

pub async fn media(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    access::container(&state, &current, &id, Permission::DepShell).await?;
    let file = ensure_media(&state, &id, &query.path).await?;
    let (kind, content_type) = preview_kind(&file_name(&query.path), b"");
    if !matches!(kind, Preview::Video | Preview::Audio) {
        return Err(ApiError::bad_request("only audio and video play from here"));
    }
    ranged_file(&file, &content_type, headers.get(header::RANGE).and_then(|v| v.to_str().ok())).await
}

pub async fn drop_media(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> ApiResult<StatusCode> {
    access::container(&state, &current, &id, Permission::DepShell).await?;
    let key = media_key(&id, &query.path);
    state.files.media.lock().unwrap().remove(&key);
    let _ = tokio::fs::remove_file(media_dir(&state).join(key)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes temp media copies older than `MEDIA_TTL`, and everything left over from before a restart.
pub fn spawn_media_sweeper(state: AppState) {
    tokio::spawn(async move {
        let dir = media_dir(&state);
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            let expired: Vec<String> = {
                let mut media = state.files.media.lock().unwrap();
                let expired: Vec<String> = media.iter().filter(|(_, at)| at.elapsed() > MEDIA_TTL).map(|(k, _)| k.clone()).collect();
                for key in &expired {
                    media.remove(key);
                }
                expired
            };
            for key in expired {
                let _ = tokio::fs::remove_file(dir.join(key)).await;
            }
        }
    });
}

#[derive(Deserialize)]
pub struct UploadQuery {
    dir: String,
    name: String,
}

pub async fn upload(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(query): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult<StatusCode> {
    access::container(&state, &current, &id, Permission::DepShell).await?;
    if query.name.is_empty() || query.name.contains('/') || query.name == ".." {
        return Err(ApiError::bad_request("invalid file name"));
    }
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(crate::db::now() as u64);
    header.set_cksum();
    let mut builder = tar::Builder::new(Vec::new());
    builder.append_data(&mut header, &query.name, body.as_ref()).map_err(anyhow::Error::from)?;
    let tar = builder.into_inner().map_err(anyhow::Error::from)?;
    let options = UploadToContainerOptionsBuilder::new().path(&query.dir).build();
    state
        .docker
        .upload_to_container(&id, Some(options), bollard::body_full(tar.into()))
        .await
        .map_err(docker_error)?;
    state.files.sizes.lock().unwrap().retain(|(c, _), _| c != &id);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_of(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, kind, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn lists_direct_children_dirs_first() {
        let tar = tar_of(&[
            ("etc/", tar::EntryType::Directory, b""),
            ("etc/passwd", tar::EntryType::Regular, b"root"),
            ("etc/ssl/", tar::EntryType::Directory, b""),
            ("etc/ssl/cert.pem", tar::EntryType::Regular, b"x"),
            ("etc/hosts", tar::EntryType::Regular, b"127"),
        ]);
        let names: Vec<_> = list_tar(tar.as_slice()).unwrap().into_iter().map(|e| (e.name, e.kind)).collect();
        assert_eq!(names, [("ssl".into(), "dir"), ("hosts".into(), "file"), ("passwd".into(), "file")]);
    }

    #[test]
    fn single_file_lists_itself() {
        let tar = tar_of(&[("hosts", tar::EntryType::Regular, b"127.0.0.1")]);
        let entries = list_tar(tar.as_slice()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].size, 9);
    }

    /// A tar whose one big file is generated on the fly, so the test fails if anything tries to keep it.
    struct HugeTar {
        header: Vec<u8>,
        body_left: u64,
        tail: Vec<u8>,
        pos: usize,
    }

    impl Read for HugeTar {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.pos < self.header.len() {
                let n = out.len().min(self.header.len() - self.pos);
                out[..n].copy_from_slice(&self.header[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            if self.body_left > 0 {
                let n = out.len().min(self.body_left as usize);
                out[..n].fill(0);
                self.body_left -= n as u64;
                return Ok(n);
            }
            let n = out.len().min(self.tail.len());
            out[..n].copy_from_slice(&self.tail[..n]);
            self.tail.drain(..n);
            Ok(n)
        }
    }

    #[test]
    fn streaming_lister_reads_past_big_files_without_keeping_them() {
        const SIZE: u64 = 256 << 20;
        let mut header = tar::Header::new_gnu();
        header.set_path("data/big.bin").unwrap();
        header.set_size(SIZE);
        header.set_mode(0o600);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        let mut dir = tar::Header::new_gnu();
        dir.set_path("data/").unwrap();
        dir.set_entry_type(tar::EntryType::Directory);
        dir.set_size(0);
        dir.set_cksum();
        let mut head = dir.as_bytes().to_vec();
        head.extend_from_slice(header.as_bytes());
        let tail = tar_of(&[("data/after.txt", tar::EntryType::Regular, b"hi"), ("data/deep/x/y", tar::EntryType::Regular, b"z")]);
        let reader = HugeTar { header: head, body_left: SIZE, tail, pos: 0 };
        let entries = list_tar(reader).unwrap();
        let names: Vec<_> = entries.iter().map(|e| (e.name.as_str(), e.size)).collect();
        assert_eq!(names, [("after.txt", 2), ("big.bin", SIZE)], "the depth 3 file is left out");
    }

    #[test]
    fn parses_gnu_and_busybox_stat_with_odd_names() {
        let gnu = "directory\t4096\t755\t1700000000\tbin dir\n\
                   regular file\t12\t644\t1700000001\tnotes\twith tab.txt\n\
                   regular empty file\t0\t600\t1700000002\t.hidden\n\
                   symbolic link\t7\t777\t1700000003\tlib\n\
                   fifo\t0\t644\t1700000004\tpipe\n\
                   LD\tusr/lib\tlib\n";
        let entries = parse_stat(gnu);
        let summary: Vec<_> = entries.iter().map(|e| (e.name.as_str(), e.kind, e.size, e.mode)).collect();
        assert_eq!(
            summary,
            [
                ("bin dir", "dir", 4096, 0o755),
                ("lib", "link", 7, 0o777),
                (".hidden", "file", 0, 0o600),
                ("notes\twith tab.txt", "file", 12, 0o644),
                ("pipe", "other", 0, 0o644),
            ]
        );
        let lib = entries.iter().find(|e| e.name == "lib").unwrap();
        assert_eq!((lib.link.as_deref(), lib.link_dir, lib.modified), (Some("usr/lib"), true, 1700000003));

        let busybox = "regular file\t5\t644\t1600000000\tapp.js\nsymbolic link\t9\t777\t1600000000\tcurrent\nLF\tapp.js\tcurrent\n";
        let entries = parse_stat(busybox);
        assert_eq!(entries[1].name, "current");
        assert_eq!((entries[1].link.as_deref(), entries[1].link_dir), (Some("app.js"), false));
        assert!(parse_stat("garbage line\n").is_empty());
    }

    #[test]
    fn parses_du_in_bytes() {
        let out = "12\tcache/\n0\tempty\n4096\tnode modules\nnot a line\n";
        assert_eq!(
            parse_du(out),
            BTreeMap::from([("cache".into(), 12 * 1024), ("empty".into(), 0), ("node modules".into(), 4096 * 1024)])
        );
    }

    #[test]
    fn content_type_and_text_sniffing() {
        assert_eq!(preview_kind("photo.JPG", b"\xff\xd8"), (Preview::Image, "image/jpeg".into()));
        assert_eq!(preview_kind("logo.svg", b"<svg"), (Preview::Svg, "image/svg+xml".into()));
        assert_eq!(preview_kind("clip.webm", b""), (Preview::Video, "video/webm".into()));
        assert_eq!(preview_kind("song.flac", b"").0, Preview::Audio);
        assert_eq!(preview_kind("doc.pdf", b"%PDF").0, Preview::Pdf);
        assert_eq!(preview_kind("index.html", b"<script>alert(1)</script>"), (Preview::Text, "text/plain; charset=utf-8".into()));
        assert_eq!(preview_kind("Makefile", b"all:\n\tcc main.c"), (Preview::Text, "text/plain; charset=utf-8".into()));
        assert_eq!(preview_kind("app.bin", b"\x7fELF\x00\x01").0, Preview::Binary);
        assert!(looks_like_text("caf\u{e9}".as_bytes()), "utf-8 text");
        assert!(looks_like_text(&"caf\u{e9}".as_bytes()[..4]), "a character cut off at the end of the sniff window");
        assert!(!looks_like_text(b"\xff\xfe\x41"), "not utf-8");
    }

    #[test]
    fn svg_and_text_previews_are_sandboxed() {
        let mut headers = HeaderMap::new();
        preview_headers(&mut headers, "image/svg+xml", Preview::Svg);
        assert_eq!(headers[header::CONTENT_SECURITY_POLICY], "sandbox");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers[header::CONTENT_DISPOSITION], "inline");
    }

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range(None, 100), Ok(None));
        assert_eq!(parse_range(Some("bytes=0-9"), 100), Ok(Some((0, 9))));
        assert_eq!(parse_range(Some("bytes=90-"), 100), Ok(Some((90, 99))));
        assert_eq!(parse_range(Some("bytes=-10"), 100), Ok(Some((90, 99))));
        assert_eq!(parse_range(Some("bytes=50-500"), 100), Ok(Some((50, 99))));
        assert_eq!(parse_range(Some("bytes=100-"), 100), Err(RangeError::Unsatisfiable));
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), Ok(None));
        assert_eq!(parse_range(Some("items=0-1"), 100), Ok(None));
    }

    #[tokio::test]
    async fn range_responses_come_from_the_temp_file() {
        use http_body_util::BodyExt;
        let path = std::env::temp_dir().join(format!("kuroe-range-{}", uuid::Uuid::new_v4()));
        tokio::fs::write(&path, b"0123456789").await.unwrap();

        let res = ranged_file(&path, "video/mp4", Some("bytes=2-5")).await.unwrap();
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(res.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
        assert_eq!(res.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(res.into_body().collect().await.unwrap().to_bytes(), "2345");

        let res = ranged_file(&path, "video/mp4", None).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CONTENT_LENGTH], "10");

        let res = ranged_file(&path, "video/mp4", Some("bytes=20-")).await.unwrap();
        assert_eq!(res.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(res.headers()[header::CONTENT_RANGE], "bytes */10");
        tokio::fs::remove_file(&path).await.unwrap();
    }
}
