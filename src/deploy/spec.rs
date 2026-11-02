use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::docker::rewrite::ServiceSettings;

pub const SINGLE_SERVICE: &str = "app";
/// The `secret_refs` key for the git access token. Env keys can't start with `@`.
pub const GIT_TOKEN_REF: &str = "@git_token";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Image {
        image: String,
        /// Id of a saved registry login, used for the pull.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        registry_identity: Option<String>,
    },
    Git {
        url: String,
        #[serde(default)]
        reference: Option<String>,
        #[serde(default)]
        context: Option<String>,
        #[serde(default)]
        dockerfile: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        /// Id of a saved Git login. Takes the place of `token`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<String>,
        /// A Compose file inside the repository. When set, the repository is deployed as a Compose stack.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compose_file: Option<String>,
    },
    Upload {
        #[serde(default)]
        dockerfile: Option<String>,
    },
    Compose {
        file: String,
    },
    /// A Dockerfile written or loaded in the UI, built with nothing else in its context.
    Dockerfile {
        file: String,
    },
    /// An uploaded archive for a static site, the source to build or the finished files. Needs `spec.site`.
    Static {},
}

impl Source {
    pub fn image(image: impl Into<String>) -> Source {
        Source::Image { image: image.into(), registry_identity: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortMapping {
    pub container: u16,
    #[serde(default)]
    pub host: Option<u16>,
    #[serde(default = "tcp")]
    pub protocol: Protocol,
}

fn tcp() -> Protocol {
    Protocol::Tcp
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContainerSpec {
    #[serde(default)]
    pub ports: Vec<PortMapping>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    #[default]
    Compose,
    Stack,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub source: Source,
    #[serde(default)]
    pub container: ContainerSpec,
    #[serde(default)]
    pub services: BTreeMap<String, ServiceSettings>,
    #[serde(default)]
    pub mode: RunMode,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub secrets: BTreeSet<String>,
    /// Env keys (and `GIT_TOKEN_REF` for the git token) that take their value from a saved secret, by its id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_refs: BTreeMap<String, String>,
    /// Set for a static site: the source is built (or not) and the output folder is served by the shared static server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<SiteOptions>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SiteOptions {
    #[serde(default)]
    pub build: SiteBuild,
    /// The folder with the finished files. Relative to the source for `none` and `command`, absolute in the image for `dockerfile`.
    /// Empty means the whole source.
    #[serde(default)]
    pub output: String,
    /// Answer paths without a file with `index.html`, for single-page apps.
    #[serde(default)]
    pub spa: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SiteBuild {
    /// The files are ready to serve.
    #[default]
    None,
    /// A command run in a builder image with the source in `/src`, like `npm ci && npm run build` in `node:26-alpine`.
    Command { image: String, command: String },
    /// The source is built as a Docker image and the output folder is copied out of it.
    Dockerfile {
        #[serde(default)]
        dockerfile: Option<String>,
    },
}

impl SiteOptions {
    /// Checks the output folder against the build kind.
    pub fn check(&self) -> Result<(), String> {
        let output = self.output.trim();
        if output.split(['/', '\\']).any(|part| part == "..") {
            return Err(format!("the output folder {output} can't contain .."));
        }
        match &self.build {
            SiteBuild::Dockerfile { .. } if !output.starts_with('/') => {
                Err("with a Dockerfile, the output folder is an absolute path in the image, like /app/dist".into())
            }
            SiteBuild::Command { .. } | SiteBuild::None if output.starts_with(['/', '\\']) => {
                Err(format!("the output folder {output} is a path inside the source, like dist"))
            }
            SiteBuild::Command { image, command } if image.trim().is_empty() || command.trim().is_empty() => {
                Err("a build step needs an image and a command".into())
            }
            _ => Ok(()),
        }
    }

    /// The output folder without leading `./` or trailing slashes, empty for the whole source.
    pub fn output_dir(&self) -> &str {
        let out = self.output.trim().trim_start_matches("./").trim_end_matches('/');
        if out == "." { "" } else { out }
    }
}

impl Spec {
    pub fn new(source: Source) -> Spec {
        Spec {
            source,
            container: Default::default(),
            services: Default::default(),
            mode: Default::default(),
            secrets: Default::default(),
            secret_refs: Default::default(),
            site: Default::default(),
        }
    }

    /// A referenced env key keeps an empty value, so a raw value can never hide behind a reference.
    pub fn clear_referenced_values(&mut self) {
        for key in self.secret_refs.keys() {
            if let Some(value) = self.container.env.get_mut(key) {
                value.clear();
            }
        }
        if self.secret_refs.contains_key(GIT_TOKEN_REF)
            && let Source::Git { token, .. } = &mut self.source
        {
            *token = None;
        }
    }

    pub fn is_compose(&self) -> bool {
        matches!(self.source, Source::Compose { .. } | Source::Git { compose_file: Some(_), .. })
    }

    pub fn is_static(&self) -> bool {
        self.site.is_some() && matches!(self.source, Source::Static {} | Source::Image { .. } | Source::Git { compose_file: None, .. })
    }

    /// Compose model for single-container sources. `build_context` is set for git and upload.
    pub fn single_service_config(&self, project: &str, build_context: Option<&str>) -> Value {
        let mut service = json!({
            "environment": self.container.env,
            "ports": self.container.ports.iter().map(|p| {
                let mut port = json!({ "mode": "ingress", "target": p.container, "protocol": p.protocol });
                if let Some(host) = p.host {
                    port["published"] = json!(host.to_string());
                }
                port
            }).collect::<Vec<_>>(),
            "networks": { "default": null },
        });
        if let Some(command) = &self.container.command {
            service["command"] = json!(command);
        }
        match (&self.source, build_context) {
            (Source::Image { image, .. }, _) => service["image"] = json!(image),
            (Source::Git { dockerfile, .. } | Source::Upload { dockerfile }, Some(context)) => {
                service["build"] = json!({
                    "context": context,
                    "dockerfile": dockerfile.as_deref().unwrap_or("Dockerfile"),
                });
            }
            (Source::Dockerfile { .. }, Some(context)) => {
                service["build"] = json!({ "context": context, "dockerfile": "Dockerfile" });
            }
            _ => {}
        }
        json!({
            "name": project,
            "services": { SINGLE_SERVICE: service },
            "networks": { "default": { "name": format!("{project}_default") } },
        })
    }

    /// Points the source at a new version: the image tag for images, the git ref for git.
    pub fn with_tag(&self, tag: &str) -> Result<Spec, String> {
        let valid = !tag.is_empty() && tag.len() <= 128 && tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
        if !valid {
            return Err(format!("{tag:?} is not a valid tag"));
        }
        let mut spec = self.clone();
        match &mut spec.source {
            Source::Image { image, .. } => *image = format!("{}:{tag}", image_repository(image)),
            Source::Git { reference, .. } => *reference = Some(tag.to_owned()),
            Source::Upload { .. } | Source::Compose { .. } | Source::Dockerfile { .. } | Source::Static {} => {
                return Err("only image and git deployments take a tag".into());
            }
        }
        Ok(spec)
    }

    pub fn masked(&self) -> Spec {
        let mut spec = self.clone();
        for value in spec.container.env.values_mut() {
            *value = "********".into();
        }
        if let Source::Git { token, .. } = &mut spec.source
            && token.is_some()
        {
            *token = Some("********".into());
        }
        spec
    }

    pub fn without_secrets(&self) -> Spec {
        let mut spec = self.clone();
        if let Source::Git { token, .. } = &mut spec.source {
            *token = None;
        }
        spec
    }
}

/// A Compose file path has to stay inside the repository.
pub fn check_compose_file(path: &str) -> Result<(), String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("the Compose file path is empty".into());
    }
    let absolute = path.starts_with(['/', '\\']) || path.as_bytes().get(1) == Some(&b':');
    if absolute {
        return Err(format!("{path} is absolute, give a path inside the repository"));
    }
    if path.split(['/', '\\']).any(|part| part == "..") {
        return Err(format!("{path} climbs out of the repository with .."));
    }
    Ok(())
}

pub fn image_repository(image: &str) -> &str {
    let image = image.split('@').next().unwrap_or(image);
    match image.rfind(':') {
        Some(colon) if !image[colon..].contains('/') => &image[..colon],
        _ => image,
    }
}

pub fn project_name(name: &str) -> String {
    format!("kuroe_{name}")
}

pub fn valid_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_source_becomes_one_service() {
        let spec = Spec {
            container: ContainerSpec {
                ports: vec![PortMapping { container: 80, host: Some(8081), protocol: Protocol::Tcp }],
                env: BTreeMap::from([("A".into(), "1".into())]),
                command: None,
            },
            ..Spec::new(Source::image("nginx:1.29"))
        };
        assert_eq!(
            spec.single_service_config("kuroe_web", None),
            json!({
                "name": "kuroe_web",
                "services": { "app": {
                    "image": "nginx:1.29",
                    "environment": { "A": "1" },
                    "ports": [{ "mode": "ingress", "target": 80, "protocol": "tcp", "published": "8081" }],
                    "networks": { "default": null }
                }},
                "networks": { "default": { "name": "kuroe_web_default" } }
            })
        );
    }

    #[test]
    fn masking_hides_env_values_and_token() {
        let spec = Spec {
            source: Source::Git {
                url: "u".into(),
                reference: None,
                context: None,
                dockerfile: None,
                token: Some("t".into()),
                identity: None,
                compose_file: None,
            },
            container: ContainerSpec { env: BTreeMap::from([("KEY".into(), "secret".into())]), ..Default::default() },
            ..Spec::new(Source::image(String::new()))
        };
        let masked = spec.masked();
        assert_eq!(masked.container.env["KEY"], "********");
        assert!(matches!(masked.source, Source::Git { token: Some(ref t), .. } if t == "********"));
    }

    #[test]
    fn tags_replace_the_image_tag_or_git_ref() {
        let image = |i: &str| Spec::new(Source::image(i));
        let retag = |i: &str| match image(i).with_tag("v2").unwrap().source {
            Source::Image { image, .. } => image,
            _ => unreachable!(),
        };
        assert_eq!(retag("ghcr.io/acme/api:v1"), "ghcr.io/acme/api:v2");
        assert_eq!(retag("ghcr.io/acme/api"), "ghcr.io/acme/api:v2");
        assert_eq!(retag("registry:5000/api"), "registry:5000/api:v2");
        assert_eq!(retag("registry:5000/api:old"), "registry:5000/api:v2");
        assert_eq!(retag("nginx@sha256:abc"), "nginx:v2");
        assert!(image("x").with_tag("bad tag").is_err());
        assert!(image("x").with_tag("").is_err());
    }

    #[test]
    fn compose_file_stays_inside_the_repository() {
        assert!(check_compose_file("compose.yaml").is_ok());
        assert!(check_compose_file("deploy/compose.prod.yaml").is_ok());
        assert!(check_compose_file("./compose.yaml").is_ok());
        assert!(check_compose_file("").is_err());
        assert!(check_compose_file("  ").is_err());
        assert!(check_compose_file("/etc/compose.yaml").is_err());
        assert!(check_compose_file("C:\\compose.yaml").is_err());
        assert!(check_compose_file("../other/compose.yaml").is_err());
        assert!(check_compose_file("deploy/../../compose.yaml").is_err());
    }

    #[test]
    fn git_with_a_compose_file_is_a_compose_deployment() {
        let git = |compose_file: Option<&str>| {
            Spec::new(Source::Git {
                url: "https://example.com/r.git".into(),
                reference: None,
                context: None,
                dockerfile: None,
                token: None,
                identity: None,
                compose_file: compose_file.map(str::to_owned),
            })
        };
        assert!(!git(None).is_compose());
        assert!(git(Some("compose.yaml")).is_compose());
        let tagged = git(Some("compose.yaml")).with_tag("v2").unwrap();
        assert!(matches!(tagged.source, Source::Git { reference: Some(ref r), compose_file: Some(_), .. } if r == "v2"));
    }

    #[test]
    fn static_sites_need_site_options_and_a_fitting_output() {
        let site = |build: SiteBuild, output: &str| SiteOptions { build, output: output.into(), spa: false };
        let node = SiteBuild::Command { image: "node:26-alpine".into(), command: "npm ci && npm run build".into() };
        assert!(site(node.clone(), "dist").check().is_ok());
        assert!(site(node.clone(), "/dist").check().is_err(), "a command's output is inside the source");
        assert!(site(node, "../dist").check().is_err());
        assert!(site(SiteBuild::Command { image: "".into(), command: "x".into() }, "dist").check().is_err());
        assert!(site(SiteBuild::Dockerfile { dockerfile: None }, "/app/dist").check().is_ok());
        assert!(site(SiteBuild::Dockerfile { dockerfile: None }, "dist").check().is_err(), "a Dockerfile's output is in the image");
        assert!(site(SiteBuild::None, "").check().is_ok());
        assert_eq!(site(SiteBuild::None, "./dist/").output_dir(), "dist");
        assert_eq!(site(SiteBuild::None, ".").output_dir(), "");

        let mut archive = Spec::new(Source::Static {});
        assert!(!archive.is_static(), "a static source without site options isn't servable");
        archive.site = Some(SiteOptions::default());
        assert!(archive.is_static());
        let mut image = Spec::new(Source::image("nginx"));
        assert!(!image.is_static(), "an image without site options runs as a container");
        image.site = Some(SiteOptions::default());
        assert!(image.is_static(), "with site options its files are copied out and served");
        assert!(!Spec::new(Source::Dockerfile { file: "FROM nginx".into() }).is_static());
    }

    #[test]
    fn names() {
        assert!(valid_name("shop-api2"));
        assert!(!valid_name("Shop"));
        assert!(!valid_name("-x"));
        assert!(!valid_name(""));
    }
}
