use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::query_parameters::KillContainerOptionsBuilder;
use futures::StreamExt;
use tokio::process::Command;

use super::{File, Kind, Reload};

const TIMEOUT: Duration = Duration::from_secs(30);

/// How the new config gets tested and loaded.
#[derive(Debug, Clone, PartialEq)]
pub enum Reloader {
    /// A shell command on this machine, like `nginx -t && nginx -s reload`. Empty means nothing to run.
    Command(String),
    /// The server runs in this container. Its own test runs there, then it gets its reload signal.
    Container { name: String, kind: Kind },
}

/// Writes the files, runs the test and reload, and puts the old files back when that fails.
pub async fn install(docker: &Docker, files: &[File], reloader: &Reloader) -> anyhow::Result<String> {
    let mut backups: Vec<(PathBuf, Option<Vec<u8>>)> = Vec::new();
    for file in files {
        let old = tokio::fs::read(&file.path).await.ok();
        backups.push((file.path.clone(), old));
    }
    let written = write_all(files).await;
    let result = match written {
        Ok(()) => reload(docker, reloader).await,
        Err(err) => Err(err),
    };
    match result {
        Ok(output) => Ok(output),
        Err(err) => {
            for (path, old) in backups {
                let _ = match old {
                    Some(bytes) => tokio::fs::write(&path, bytes).await,
                    None => tokio::fs::remove_file(&path).await,
                };
            }
            Err(err.context("the previous config was put back"))
        }
    }
}

async fn write_all(files: &[File]) -> anyhow::Result<()> {
    for file in files {
        if let Some(parent) = file.path.parent() {
            tokio::fs::create_dir_all(parent).await.with_context(|| format!("creating {}", parent.display()))?;
        }
        tokio::fs::write(&file.path, &file.body).await.with_context(|| format!("writing {}", file.path.display()))?;
    }
    Ok(())
}

async fn reload(docker: &Docker, reloader: &Reloader) -> anyhow::Result<String> {
    match reloader {
        Reloader::Command(command) if command.trim().is_empty() => Ok(String::new()),
        Reloader::Command(command) => run_shell(command).await,
        Reloader::Container { name, kind } => {
            let (test, how) = kind.container_reload();
            let mut output = String::new();
            if let Some(test) = test {
                output.push_str(&exec(docker, name, test).await?);
            }
            match how {
                Reload::Signal(signal) => {
                    let options = KillContainerOptionsBuilder::new().signal(signal).build();
                    docker.kill_container(name, Some(options)).await.with_context(|| format!("sending {signal} to {name}"))?;
                }
                Reload::Exec(cmd) => output.push_str(&exec(docker, name, cmd).await?),
                Reload::None => {}
            }
            Ok(output)
        }
    }
}

pub async fn run_shell(command: &str) -> anyhow::Result<String> {
    let run = Command::new("sh").arg("-c").arg(command).kill_on_drop(true).output();
    let output = tokio::time::timeout(TIMEOUT, run).await.with_context(|| format!("{command} took longer than 30 seconds"))??;
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)).trim().to_owned();
    if !output.status.success() {
        bail!("{command} failed: {text}");
    }
    Ok(text)
}

async fn exec(docker: &Docker, container: &str, cmd: &[&str]) -> anyhow::Result<String> {
    let exec = docker
        .create_exec(
            container,
            CreateExecOptions {
                cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                ..Default::default()
            },
        )
        .await
        .with_context(|| format!("running {} in {container}", cmd.join(" ")))?;
    let mut text = String::new();
    if let StartExecResults::Attached { mut output, .. } = docker.start_exec(&exec.id, None).await? {
        while let Some(Ok(chunk)) = output.next().await {
            text.push_str(&chunk.to_string());
        }
    }
    let code = docker.inspect_exec(&exec.id).await?.exit_code.unwrap_or(0);
    if code != 0 {
        bail!("{} in {container} failed: {}", cmd.join(" "), text.trim());
    }
    Ok(text.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &std::path::Path, name: &str, body: &str) -> File {
        File { path: dir.join(name), body: body.into() }
    }

    #[tokio::test]
    async fn a_failed_test_puts_the_old_files_back() {
        let dir = std::env::temp_dir().join(format!("kuroe-ws-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join("kuroe.conf"), "old").await.unwrap();
        let docker = crate::docker::connect().unwrap();
        let files = [file(&dir, "kuroe.conf", "new"), file(&dir, "stream.conf", "added")];

        let err = install(&docker, &files, &Reloader::Command("echo config broken >&2; exit 1".into())).await.unwrap_err();
        assert!(format!("{err:#}").contains("config broken"), "{err:#}");
        assert_eq!(tokio::fs::read_to_string(dir.join("kuroe.conf")).await.unwrap(), "old");
        assert!(!dir.join("stream.conf").exists(), "a file that wasn't there before is removed again");

        let out = install(&docker, &files, &Reloader::Command("echo reloaded".into())).await.unwrap();
        assert_eq!(out, "reloaded");
        assert_eq!(tokio::fs::read_to_string(dir.join("kuroe.conf")).await.unwrap(), "new");
        assert_eq!(tokio::fs::read_to_string(dir.join("stream.conf")).await.unwrap(), "added");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
