use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use bollard::Docker;
use bollard::query_parameters::EventsOptionsBuilder;
use futures::StreamExt;
use serde::Serialize;
use tokio::sync::broadcast;

use crate::auth::{Access, Permission};
use crate::docker::rewrite::{LABEL_DEPLOYMENT, LABEL_MANAGED};
use crate::runners::LABEL_POOL;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind")]
pub enum Event {
    #[serde(rename = "deployment.status")]
    DeploymentStatus { id: String, status: String, revision: i64 },
    #[serde(rename = "deployment.changed")]
    DeploymentChanged { id: String },
    #[serde(rename = "container.state")]
    ContainerState { deployment: String, container: String, state: String },
    #[serde(rename = "proxy.traefik")]
    ProxyTraefik { running: bool },
    #[serde(rename = "runner.instance")]
    RunnerInstance { pool: String, container: String, state: String },
}

impl Event {
    pub fn visible_to(&self, access: &Access) -> bool {
        match self {
            Event::DeploymentStatus { id, .. } | Event::DeploymentChanged { id } => access.can(id, Permission::DepView),
            Event::ContainerState { deployment, .. } => access.can(deployment, Permission::DepView),
            Event::ProxyTraefik { .. } => true,
            Event::RunnerInstance { .. } => access.has(Permission::Runners),
        }
    }
}

static CHANNEL: LazyLock<broadcast::Sender<Event>> = LazyLock::new(|| broadcast::channel(256).0);

pub fn send(event: Event) {
    let _ = CHANNEL.send(event);
}

pub fn subscribe() -> broadcast::Receiver<Event> {
    CHANNEL.subscribe()
}

pub fn changed(deployment_id: &str) {
    send(Event::DeploymentChanged { id: deployment_id.to_owned() });
}

fn container_state(action: &str) -> Option<&'static str> {
    match action {
        "start" => Some("running"),
        "die" => Some("exited"),
        _ => None,
    }
}

pub fn from_docker(action: &str, container: &str, attributes: &HashMap<String, String>) -> Option<Event> {
    let state = container_state(action)?;
    if attributes.get("name").map(String::as_str) == Some(crate::proxy::traefik::CONTAINER) {
        return Some(Event::ProxyTraefik { running: state == "running" });
    }
    if let Some(deployment) = attributes.get(LABEL_DEPLOYMENT) {
        return Some(Event::ContainerState { deployment: deployment.clone(), container: container.to_owned(), state: state.to_owned() });
    }
    attributes
        .get(LABEL_POOL)
        .map(|pool| Event::RunnerInstance { pool: pool.clone(), container: container.to_owned(), state: state.to_owned() })
}

pub fn spawn_docker_watcher(docker: Docker) {
    tokio::spawn(async move {
        loop {
            watch(&docker).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

async fn watch(docker: &Docker) {
    let managed = format!("{LABEL_MANAGED}=true");
    let filters = HashMap::from([
        ("type", vec!["container"]),
        ("event", vec!["start", "die"]),
        ("label", vec![managed.as_str()]),
    ]);
    let mut events = docker.events(Some(EventsOptionsBuilder::new().filters(&filters).build()));
    while let Some(event) = events.next().await {
        let event = match event {
            Ok(event) => event,
            Err(err) => {
                tracing::warn!("docker events for live updates stopped: {err}");
                return;
            }
        };
        let (Some(action), Some(actor)) = (event.action, event.actor) else { continue };
        let (Some(id), Some(attributes)) = (actor.id, actor.attributes) else { continue };
        if let Some(event) = from_docker(&action, &id, &attributes) {
            send(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn docker_events_map_to_live_events() {
        let app = labels(&[("name", "kuroe_shop-blue-app-1"), (LABEL_DEPLOYMENT, "d1")]);
        assert_eq!(
            from_docker("die", "c1", &app),
            Some(Event::ContainerState { deployment: "d1".into(), container: "c1".into(), state: "exited".into() })
        );
        assert_eq!(from_docker("start", "t1", &labels(&[("name", "kuroe_traefik")])), Some(Event::ProxyTraefik { running: true }));
        assert_eq!(
            from_docker("start", "r1", &labels(&[(LABEL_POOL, "p1")])),
            Some(Event::RunnerInstance { pool: "p1".into(), container: "r1".into(), state: "running".into() })
        );
        assert_eq!(from_docker("exec_start", "c1", &app), None);
        assert_eq!(from_docker("start", "s1", &labels(&[("name", "kuroe_static")])), None);
    }

    #[test]
    fn events_serialize_with_their_kind() {
        let json = serde_json::to_value(Event::DeploymentStatus { id: "d1".into(), status: "running".into(), revision: 3 }).unwrap();
        assert_eq!(json, serde_json::json!({ "kind": "deployment.status", "id": "d1", "status": "running", "revision": 3 }));
    }
}
