pub mod binary;
pub mod container;
pub mod release;

use bollard::Docker;
use serde::Serialize;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUILD: Option<&str> = option_env!("KUROE_BUILD");

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum InstallMode {
    Container { id: String, name: String, image: String },
    Binary,
    Unsupported,
}

pub fn container_id_from_mountinfo(mountinfo: &str) -> Option<String> {
    mountinfo.lines().find_map(|line| {
        let start = line.find("/containers/")? + "/containers/".len();
        let id = line.get(start..start + 64)?;
        (id.chars().all(|c| c.is_ascii_hexdigit()) && line[start + 64..].starts_with('/')).then(|| id.to_owned())
    })
}

pub async fn detect(docker: &Docker) -> InstallMode {
    let own = std::fs::read_to_string("/proc/self/mountinfo").ok().and_then(|m| container_id_from_mountinfo(&m));
    if let Some(id) = own
        && let Ok(inspect) = docker.inspect_container(&id, None).await
    {
        return InstallMode::Container {
            id,
            name: inspect.name.unwrap_or_default().trim_start_matches('/').to_owned(),
            image: inspect.config.and_then(|c| c.image).unwrap_or_default(),
        };
    }
    if cfg!(target_os = "linux") { InstallMode::Binary } else { InstallMode::Unsupported }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_own_container_id_in_mountinfo() {
        let id = "3f1c2b4a5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708";
        assert_eq!(id.len(), 64);
        let mountinfo = format!(
            "612 594 0:56 / / rw,relatime master:310 - overlay overlay rw\n\
             639 612 8:32 /docker/containers/{id}/hostname /etc/hostname rw,relatime - ext4 /dev/sdc rw\n"
        );
        assert_eq!(container_id_from_mountinfo(&mountinfo).as_deref(), Some(id));
        assert_eq!(container_id_from_mountinfo("612 594 0:56 / / rw - ext4 /dev/sda rw\n"), None);
    }
}
