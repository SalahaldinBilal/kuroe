use std::collections::{BTreeSet, HashMap};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::User;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Role(pub String);

impl Role {
    pub const BUILTIN: [&str; 4] = ["owner", "admin", "deployer", "viewer"];

    pub fn owner() -> Self {
        Role("owner".into())
    }
    pub fn admin() -> Self {
        Role("admin".into())
    }
    pub fn deployer() -> Self {
        Role("deployer".into())
    }
    pub fn viewer() -> Self {
        Role("viewer".into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_owner(&self) -> bool {
        self.0 == "owner"
    }
}

impl From<String> for Role {
    fn from(value: String) -> Self {
        Role(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "dep.view")]
    DepView,
    #[serde(rename = "dep.create")]
    DepCreate,
    #[serde(rename = "dep.deploy")]
    DepDeploy,
    #[serde(rename = "dep.edit")]
    DepEdit,
    #[serde(rename = "dep.secrets")]
    DepSecrets,
    #[serde(rename = "dep.shell")]
    DepShell,
    #[serde(rename = "dep.domains")]
    DepDomains,
    #[serde(rename = "dep.ci")]
    DepCi,
    #[serde(rename = "dep.delete")]
    DepDelete,
    #[serde(rename = "vol")]
    Volumes,
    #[serde(rename = "proxy")]
    Proxy,
    #[serde(rename = "ident.use")]
    IdentUse,
    #[serde(rename = "ident.manage")]
    IdentManage,
    #[serde(rename = "secret.use")]
    SecretUse,
    #[serde(rename = "secret.manage")]
    SecretManage,
    #[serde(rename = "settings")]
    Settings,
    #[serde(rename = "import")]
    Import,
    #[serde(rename = "users")]
    Users,
    #[serde(rename = "runners")]
    Runners,
}

impl Permission {
    pub const ALL: [Permission; 19] = [
        Permission::DepView,
        Permission::DepCreate,
        Permission::DepDeploy,
        Permission::DepEdit,
        Permission::DepSecrets,
        Permission::DepShell,
        Permission::DepDomains,
        Permission::DepCi,
        Permission::DepDelete,
        Permission::Volumes,
        Permission::Proxy,
        Permission::IdentUse,
        Permission::IdentManage,
        Permission::SecretUse,
        Permission::SecretManage,
        Permission::Settings,
        Permission::Import,
        Permission::Users,
        Permission::Runners,
    ];

    /// Permissions that apply per deployment and follow a role's scope and grants.
    pub fn on_deployment(self) -> bool {
        matches!(
            self,
            Permission::DepView
                | Permission::DepDeploy
                | Permission::DepEdit
                | Permission::DepSecrets
                | Permission::DepShell
                | Permission::DepDomains
                | Permission::DepCi
                | Permission::DepDelete
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    /// The role's deployment permissions only apply where the user has a grant. Elsewhere they can only view.
    Granted,
    Some(BTreeSet<String>),
}

impl Scope {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(match text {
            "all" => Scope::All,
            "granted" => Scope::Granted,
            list => Scope::Some(serde_json::from_str(list).with_context(|| format!("role scope {list:?}"))?),
        })
    }

    pub fn to_db(&self) -> String {
        match self {
            Scope::All => "all".into(),
            Scope::Granted => "granted".into(),
            Scope::Some(ids) => serde_json::to_string(ids).expect("a set of strings serializes"),
        }
    }
}

impl Serialize for Scope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Scope::All => serializer.serialize_str("all"),
            Scope::Granted => serializer.serialize_str("granted"),
            Scope::Some(ids) => ids.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Scope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Word(String),
            List(BTreeSet<String>),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Word(w) if w == "all" => Ok(Scope::All),
            Raw::Word(w) if w == "granted" => Ok(Scope::Granted),
            Raw::Word(w) => Err(serde::de::Error::custom(format!("unknown scope {w}, use \"all\" or a list"))),
            Raw::List(ids) => Ok(Scope::Some(ids)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoleDef {
    pub name: String,
    pub builtin: bool,
    pub description: String,
    pub permissions: BTreeSet<Permission>,
    pub scope: Scope,
}

#[derive(sqlx::FromRow)]
struct RoleRow {
    name: String,
    builtin: bool,
    description: String,
    permissions: String,
    scope: String,
}

impl TryFrom<RoleRow> for RoleDef {
    type Error = anyhow::Error;

    fn try_from(row: RoleRow) -> anyhow::Result<Self> {
        Ok(RoleDef {
            permissions: serde_json::from_str(&row.permissions).with_context(|| format!("permissions of role {}", row.name))?,
            scope: Scope::parse(&row.scope)?,
            name: row.name,
            builtin: row.builtin,
            description: row.description,
        })
    }
}

impl RoleDef {
    pub async fn load(db: &SqlitePool, name: &str) -> anyhow::Result<Option<RoleDef>> {
        let row: Option<RoleRow> = sqlx::query_as("SELECT name, builtin, description, permissions, scope FROM roles WHERE name = ?")
            .bind(name)
            .fetch_optional(db)
            .await?;
        row.map(RoleDef::try_from).transpose()
    }

    pub async fn list(db: &SqlitePool) -> anyhow::Result<Vec<RoleDef>> {
        let rows: Vec<RoleRow> =
            sqlx::query_as("SELECT name, builtin, description, permissions, scope FROM roles ORDER BY builtin DESC, name")
                .fetch_all(db)
                .await?;
        rows.into_iter().map(RoleDef::try_from).collect()
    }

    fn deployment_permissions(&self) -> BTreeSet<Permission> {
        self.permissions.iter().copied().filter(|p| p.on_deployment()).collect()
    }
}

/// What one user may do, loaded once per request.
#[derive(Debug, Clone)]
pub struct Access {
    owner: bool,
    role: RoleDef,
    grants: HashMap<String, RoleDef>,
}

impl Access {
    pub async fn load(db: &SqlitePool, user: &User) -> anyhow::Result<Access> {
        let role = RoleDef::load(db, user.role.as_str()).await?.with_context(|| format!("role {} does not exist", user.role.as_str()))?;
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT deployment_id, role FROM deployment_grants WHERE user_id = ?").bind(&user.id).fetch_all(db).await?;
        let mut grants = HashMap::new();
        for (deployment, name) in rows {
            if let Some(def) = RoleDef::load(db, &name).await? {
                grants.insert(deployment, def);
            }
        }
        Ok(Access { owner: user.role.is_owner(), role, grants })
    }

    pub fn is_owner(&self) -> bool {
        self.owner
    }

    pub fn role(&self) -> &RoleDef {
        &self.role
    }

    /// Server-wide permissions come from the user's own role.
    pub fn has(&self, permission: Permission) -> bool {
        self.owner || self.role.permissions.contains(&permission)
    }

    /// Covers every deployment, including containers that belong to none.
    pub fn everywhere(&self, permission: Permission) -> bool {
        self.owner || (self.role.scope == Scope::All && self.role.permissions.contains(&permission))
    }

    pub fn on(&self, deployment_id: &str) -> BTreeSet<Permission> {
        if self.owner {
            return Permission::ALL.into_iter().filter(|p| p.on_deployment()).collect();
        }
        if let Some(grant) = self.grants.get(deployment_id) {
            return grant.deployment_permissions();
        }
        match &self.role.scope {
            Scope::All => self.role.deployment_permissions(),
            Scope::Some(ids) if ids.contains(deployment_id) => self.role.deployment_permissions(),
            Scope::Some(_) => BTreeSet::new(),
            Scope::Granted => self.role.deployment_permissions().into_iter().filter(|p| *p == Permission::DepView).collect(),
        }
    }

    pub fn can(&self, deployment_id: &str, permission: Permission) -> bool {
        self.on(deployment_id).contains(&permission)
    }

    /// A user whose role doesn't cover a new deployment gets a grant on it, so they can use what they made.
    pub fn needs_grant_on_new(&self) -> bool {
        !self.owner && self.role.scope != Scope::All
    }
}
