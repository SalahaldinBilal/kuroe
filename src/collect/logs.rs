use std::path::{Path, PathBuf};

use bollard::Docker;
use bollard::container::LogOutput;
use bollard::query_parameters::LogsOptionsBuilder;
use futures::StreamExt;
use sqlx::SqlitePool;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::io::AsyncWriteExt;

use super::retention::DATE_FORMAT;

pub fn day_file(logs_dir: &Path, deployment_id: &str, service: &str, date: &str) -> PathBuf {
    logs_dir.join(deployment_id).join(service).join(format!("{date}.log"))
}

#[derive(Debug, PartialEq)]
pub struct Line {
    pub at: OffsetDateTime,
    pub stamp: String,
    pub text: String,
}

/// Splits one Docker log line of the form `<rfc3339nano> <message>`.
pub fn parse_line(raw: &str) -> Option<Line> {
    let (stamp, text) = raw.split_once(' ').unwrap_or((raw, ""));
    let at = OffsetDateTime::parse(stamp, &Rfc3339).ok()?;
    Some(Line { at, stamp: stamp.to_owned(), text: text.to_owned() })
}

pub fn format_line(line: &Line, stream: &str, container_id: &str) -> String {
    let short = &container_id[..container_id.len().min(12)];
    format!("{} {stream} {short} {}\n", line.stamp, line.text)
}

async fn cursor(db: &SqlitePool, container_id: &str) -> sqlx::Result<Option<OffsetDateTime>> {
    let stamp: Option<String> =
        sqlx::query_scalar("SELECT last_ts FROM log_cursors WHERE container_id = ?").bind(container_id).fetch_optional(db).await?;
    Ok(stamp.and_then(|s| OffsetDateTime::parse(&s, &Rfc3339).ok()))
}

async fn save_cursor(db: &SqlitePool, container_id: &str, stamp: &str) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO log_cursors (container_id, last_ts) VALUES (?, ?) ON CONFLICT (container_id) DO UPDATE SET last_ts = excluded.last_ts")
        .bind(container_id)
        .bind(stamp)
        .execute(db)
        .await?;
    Ok(())
}

const SEARCH_BYTES: u64 = 1 << 20;
const SAVE_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// Newest line this container already has on disk, looked up in today's and yesterday's file.
pub fn last_written(logs_dir: &Path, deployment_id: &str, service: &str, container_id: &str, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let short = &container_id[..container_id.len().min(12)];
    for day in [now.date(), now.date().previous_day()?] {
        let path = day_file(logs_dir, deployment_id, service, &day.format(DATE_FORMAT).ok()?);
        let Ok(text) = tail(&path, SEARCH_BYTES) else { continue };
        let newest = text
            .lines()
            .filter(|l| l.split(' ').nth(2) == Some(short))
            .filter_map(|l| OffsetDateTime::parse(l.split(' ').next()?, &Rfc3339).ok())
            .max();
        if newest.is_some() {
            return newest;
        }
    }
    None
}

pub async fn follow(db: SqlitePool, docker: Docker, logs_dir: PathBuf, deployment_id: String, service: String, container_id: String) -> anyhow::Result<()> {
    let from_file = {
        let (dir, dep, svc, id) = (logs_dir.clone(), deployment_id.clone(), service.clone(), container_id.clone());
        tokio::task::spawn_blocking(move || last_written(&dir, &dep, &svc, &id, OffsetDateTime::now_utc())).await?
    };
    let last = cursor(&db, &container_id).await?.max(from_file);
    let mut options = LogsOptionsBuilder::new().follow(true).stdout(true).stderr(true).timestamps(true);
    if let Some(last) = last {
        options = options.since(last.unix_timestamp() as i32);
    }
    let mut stream = docker.logs(&container_id, Some(options.build()));
    let mut partial = [String::new(), String::new()];
    let mut open: Option<(String, tokio::fs::File)> = None;
    let mut newest: Option<String> = None;
    let mut saved = std::time::Instant::now();

    while let Some(chunk) = stream.next().await {
        let (index, stream_name, bytes) = match chunk? {
            LogOutput::StdErr { message } => (1, "stderr", message),
            LogOutput::StdOut { message } | LogOutput::Console { message } => (0, "stdout", message),
            LogOutput::StdIn { .. } => continue,
        };
        partial[index].push_str(&String::from_utf8_lossy(&bytes));
        while let Some(end) = partial[index].find('\n') {
            let raw: String = partial[index].drain(..=end).collect();
            let Some(line) = parse_line(raw.trim_end_matches(['\n', '\r'])) else { continue };
            if last.is_some_and(|last| line.at <= last) {
                continue;
            }
            let date = line.at.date().format(DATE_FORMAT)?;
            if open.as_ref().is_none_or(|(d, _)| *d != date) {
                let path = day_file(&logs_dir, &deployment_id, &service, &date);
                tokio::fs::create_dir_all(path.parent().unwrap()).await?;
                let file = tokio::fs::OpenOptions::new().create(true).append(true).open(&path).await?;
                open = Some((date, file));
            }
            let (_, file) = open.as_mut().unwrap();
            file.write_all(format_line(&line, stream_name, &container_id).as_bytes()).await?;
            newest = Some(line.stamp);
        }
        if saved.elapsed() >= SAVE_EVERY
            && let Some(stamp) = &newest
        {
            save_cursor(&db, &container_id, stamp).await?;
            saved = std::time::Instant::now();
        }
    }
    if let Some(stamp) = &newest {
        save_cursor(&db, &container_id, stamp).await?;
    }
    Ok(())
}

pub fn tail(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Ok(if start > 0 { text.split_once('\n').map(|(_, rest)| rest.to_owned()).unwrap_or_default() } else { text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_docker_timestamped_line() {
        let line = parse_line("2026-10-31T10:00:00.123456789Z GET / 200").unwrap();
        assert_eq!(line.stamp, "2026-10-31T10:00:00.123456789Z");
        assert_eq!(line.text, "GET / 200");
        assert_eq!(format_line(&line, "stdout", "0123456789abcdef"), "2026-10-31T10:00:00.123456789Z stdout 0123456789ab GET / 200\n");
    }

    #[test]
    fn rejects_lines_without_a_timestamp() {
        assert!(parse_line("no timestamp here").is_none());
    }

    #[test]
    fn resumes_after_this_containers_newest_line_on_disk() {
        let dir = std::env::temp_dir().join(format!("kuroe-resume-{}", uuid::Uuid::new_v4()));
        let now = OffsetDateTime::parse("2026-10-31T00:00:05Z", &Rfc3339).unwrap();
        let yesterday = day_file(&dir, "dep", "web", "2026-10-30");
        std::fs::create_dir_all(yesterday.parent().unwrap()).unwrap();
        std::fs::write(
            &yesterday,
            "2026-10-30T23:59:58.1Z stdout aaaaaaaaaaaa one\n\
             2026-10-30T23:59:59.5Z stdout bbbbbbbbbbbb other replica\n\
             2026-10-30T23:59:59.2Z stdout aaaaaaaaaaaa two\n",
        )
        .unwrap();
        let found = last_written(&dir, "dep", "web", "aaaaaaaaaaaa1234", now).unwrap();
        assert_eq!(found, OffsetDateTime::parse("2026-10-30T23:59:59.2Z", &Rfc3339).unwrap());

        std::fs::write(day_file(&dir, "dep", "web", "2026-10-31"), "2026-10-31T00:00:01.0Z stdout aaaaaaaaaaaa three\n").unwrap();
        let found = last_written(&dir, "dep", "web", "aaaaaaaaaaaa1234", now).unwrap();
        assert_eq!(found, OffsetDateTime::parse("2026-10-31T00:00:01Z", &Rfc3339).unwrap());
        assert!(last_written(&dir, "dep", "web", "cccccccccccc", now).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tail_starts_on_a_line_boundary() {
        let path = std::env::temp_dir().join(format!("kuroe-tail-{}.log", uuid::Uuid::new_v4()));
        std::fs::write(&path, "first line\nsecond line\nthird\n").unwrap();
        assert_eq!(tail(&path, 15).unwrap(), "third\n");
        assert_eq!(tail(&path, 1000).unwrap(), "first line\nsecond line\nthird\n");
        std::fs::remove_file(&path).unwrap();
    }
}
