pub mod admission;
pub mod netlimit;
pub mod rewrite;

use bollard::Docker;
use bollard::models::{LocalNodeState, SystemInfo};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct DaemonInfo {
    pub server_version: Option<String>,
    pub api_version: Option<String>,
    pub operating_system: Option<String>,
    pub architecture: Option<String>,
    pub cpus: Option<i64>,
    pub memory_bytes: Option<i64>,
    pub containers: Option<i64>,
    pub containers_running: Option<i64>,
    pub images: Option<i64>,
    pub swarm: Option<String>,
    pub swarm_manager: bool,
}

pub fn connect() -> anyhow::Result<Docker> {
    Ok(Docker::connect_with_defaults()?)
}

/// Stacks can only be deployed from an active swarm manager.
pub fn swarm_manager(info: &SystemInfo) -> bool {
    info.swarm
        .as_ref()
        .is_some_and(|s| s.local_node_state == Some(LocalNodeState::ACTIVE) && s.control_available == Some(true))
}

pub async fn is_swarm_manager(docker: &Docker) -> anyhow::Result<bool> {
    Ok(swarm_manager(&docker.info().await?))
}

pub async fn daemon_info(docker: &Docker) -> anyhow::Result<DaemonInfo> {
    let info = docker.info().await?;
    let version = docker.version().await?;
    let swarm_manager = swarm_manager(&info);
    Ok(DaemonInfo {
        server_version: info.server_version,
        api_version: version.api_version,
        operating_system: info.operating_system,
        architecture: info.architecture,
        cpus: info.ncpu,
        memory_bytes: info.mem_total,
        containers: info.containers,
        containers_running: info.containers_running,
        images: info.images,
        swarm: info.swarm.and_then(|s| s.local_node_state).map(|s| s.to_string()),
        swarm_manager,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(state: &str, manager: bool) -> SystemInfo {
        serde_json::from_value(serde_json::json!({ "Swarm": { "LocalNodeState": state, "ControlAvailable": manager } })).unwrap()
    }

    #[test]
    fn stacks_need_an_active_manager() {
        assert!(swarm_manager(&info("active", true)));
        assert!(!swarm_manager(&info("active", false)), "a worker node can't deploy stacks");
        assert!(!swarm_manager(&info("inactive", false)));
        assert!(!swarm_manager(&info("pending", true)));
        assert!(!swarm_manager(&SystemInfo::default()));
    }
}
