pub mod history;
pub mod runner;
pub mod spec;
pub mod tokens;

use serde::Serialize;
use sqlx::SqlitePool;

use crate::auth::{Access, User};
use crate::db::now;
pub use spec::{Source, Spec};

#[derive(Debug, Clone, Serialize)]
pub struct Deployment {
    pub id: String,
    pub name: String,
    pub spec: Spec,
    pub status: String,
    pub revision: i64,
    pub last_output: String,
    pub active_project: Option<String>,
    pub created_by: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    name: String,
    spec: String,
    status: String,
    revision: i64,
    last_output: String,
    active_project: Option<String>,
    created_by: Option<String>,
    created_at: i64,
    updated_at: i64,
}

impl TryFrom<Row> for Deployment {
    type Error = anyhow::Error;

    fn try_from(row: Row) -> anyhow::Result<Self> {
        Ok(Deployment {
            spec: crate::secrets::open_spec(&row.id, &serde_json::from_str(&row.spec)?)?,
            id: row.id,
            name: row.name,
            status: row.status,
            revision: row.revision,
            last_output: row.last_output,
            active_project: row.active_project,
            created_by: row.created_by,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

/// Saves a new push webhook secret, sealed, and returns it in plain text.
pub async fn new_webhook_secret(db: &SqlitePool, id: &str) -> sqlx::Result<String> {
    let secret = tokens::random_secret(24);
    save_webhook_secret(db, id, &secret).await?;
    Ok(secret)
}

pub async fn save_webhook_secret(db: &SqlitePool, id: &str, secret: &str) -> sqlx::Result<()> {
    let sealed = crate::secrets::sealer().seal(&crate::secrets::webhook_aad(id), secret);
    sqlx::query("UPDATE deployments SET webhook_secret = ? WHERE id = ?").bind(sealed).bind(id).execute(db).await?;
    Ok(())
}

pub async fn has_webhook(db: &SqlitePool, id: &str) -> sqlx::Result<bool> {
    let secret: Option<Option<String>> =
        sqlx::query_scalar("SELECT webhook_secret FROM deployments WHERE id = ?").bind(id).fetch_optional(db).await?;
    Ok(secret.flatten().is_some())
}

pub async fn create(db: &SqlitePool, name: &str, spec: &Spec, created_by: &User) -> anyhow::Result<Deployment> {
    let id = uuid::Uuid::new_v4().to_string();
    let ts = now();
    let grant_creator = Access::load(db, created_by).await?.needs_grant_on_new();
    let mut tx = db.begin().await?;
    sqlx::query(
        "INSERT INTO deployments (id, name, spec, status, created_by, created_at, updated_at) VALUES (?, ?, ?, 'created', ?, ?, ?)",
    )
    .bind(&id)
    .bind(name)
    .bind(serde_json::to_string(&crate::secrets::seal_spec(&id, spec))?)
    .bind(&created_by.id)
    .bind(ts)
    .bind(ts)
    .execute(&mut *tx)
    .await?;
    if grant_creator {
        sqlx::query("INSERT INTO deployment_grants (deployment_id, user_id, role) VALUES (?, ?, ?)")
            .bind(&id)
            .bind(&created_by.id)
            .bind(created_by.role.as_str())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    crate::events::changed(&id);
    get(db, &id).await?.ok_or_else(|| anyhow::anyhow!("deployment vanished after insert"))
}

pub async fn get(db: &SqlitePool, id: &str) -> anyhow::Result<Option<Deployment>> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, name, spec, status, revision, last_output, active_project, created_by, created_at, updated_at
         FROM deployments WHERE id = ?",
    )
    .bind(id)
        .fetch_optional(db)
        .await?;
    row.map(Deployment::try_from).transpose()
}

pub async fn list(db: &SqlitePool) -> anyhow::Result<Vec<Deployment>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, name, spec, status, revision, last_output, active_project, created_by, created_at, updated_at
         FROM deployments ORDER BY name",
    )
    .fetch_all(db)
    .await?;
    rows.into_iter().map(Deployment::try_from).collect()
}

pub async fn update_spec(db: &SqlitePool, id: &str, spec: &Spec) -> anyhow::Result<()> {
    sqlx::query("UPDATE deployments SET spec = ?, updated_at = ? WHERE id = ?")
        .bind(serde_json::to_string(&crate::secrets::seal_spec(id, spec))?)
        .bind(now())
        .bind(id)
        .execute(db)
        .await?;
    crate::events::changed(id);
    Ok(())
}

fn status_event(id: &str, status: &str, revision: i64) {
    crate::events::send(crate::events::Event::DeploymentStatus { id: id.to_owned(), status: status.to_owned(), revision });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Begun {
    pub revision: i64,
    /// The id of the deploy's history row.
    pub deploy: String,
}

/// None when a deploy is already running.
pub async fn begin_deploy(
    db: &SqlitePool,
    id: &str,
    started: &history::Started,
    source: &history::DeploySource,
) -> anyhow::Result<Option<Begun>> {
    let mut tx = db.begin().await?;
    let revision: Option<i64> = sqlx::query_scalar(
        "UPDATE deployments SET status = 'deploying', revision = revision + 1, last_output = '', updated_at = ?
         WHERE id = ? AND status != 'deploying' RETURNING revision",
    )
    .bind(now())
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(revision) = revision else { return Ok(None) };
    let deploy = history::insert(&mut tx, id, revision, started, source).await?;
    tx.commit().await?;
    status_event(id, "deploying", revision);
    Ok(Some(Begun { revision, deploy }))
}

pub async fn finish(db: &SqlitePool, id: &str, status: &str, output: &str) -> sqlx::Result<()> {
    let revision: Option<i64> =
        sqlx::query_scalar("UPDATE deployments SET status = ?, last_output = ?, updated_at = ? WHERE id = ? RETURNING revision")
            .bind(status)
            .bind(output)
            .bind(now())
            .bind(id)
            .fetch_optional(db)
            .await?;
    if let Some(revision) = revision {
        status_event(id, status, revision);
    }
    Ok(())
}

pub async fn set_active_project(db: &SqlitePool, id: &str, project: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE deployments SET active_project = ? WHERE id = ?").bind(project).bind(id).execute(db).await?;
    Ok(())
}

pub async fn delete(db: &SqlitePool, id: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM deployments WHERE id = ?").bind(id).execute(db).await?;
    crate::events::changed(id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Permission as P, Role, create_user};
    use crate::db::test_pool;

    fn spec() -> Spec {
        Spec::new(Source::image("nginx"))
    }

    async fn can(db: &SqlitePool, user: &User, deployment: &str, permission: P) -> bool {
        Access::load(db, user).await.unwrap().can(deployment, permission)
    }

    #[tokio::test]
    async fn deployer_gets_grant_on_create_and_only_there() {
        let db = test_pool().await;
        let dev = create_user(&db, "dev", "dev-password-1", Role::deployer()).await.unwrap();
        let other = create_user(&db, "other", "other-password-1", Role::deployer()).await.unwrap();
        let viewer = create_user(&db, "eye", "eye-password-1", Role::viewer()).await.unwrap();
        let admin = create_user(&db, "adm", "adm-password-1", Role::admin()).await.unwrap();
        let d = create(&db, "shop", &spec(), &dev).await.unwrap();
        let by_admin = create(&db, "blog", &spec(), &admin).await.unwrap();

        assert!(can(&db, &dev, &d.id, P::DepShell).await);
        assert!(!can(&db, &dev, &d.id, P::DepDelete).await);
        assert!(!can(&db, &other, &d.id, P::DepDeploy).await);
        assert!(can(&db, &other, &d.id, P::DepView).await);
        assert!(can(&db, &viewer, &d.id, P::DepView).await);
        assert!(!can(&db, &viewer, &d.id, P::DepShell).await);
        let grants: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM deployment_grants WHERE deployment_id = ?")
            .bind(&by_admin.id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(grants, 0, "an admin's role already covers every deployment");
    }

    const DEPLOYMENT: [P; 8] =
        [P::DepView, P::DepDeploy, P::DepEdit, P::DepSecrets, P::DepShell, P::DepDomains, P::DepCi, P::DepDelete];
    const SERVER: [P; 6] = [P::DepCreate, P::Volumes, P::Proxy, P::Settings, P::Import, P::Users];

    fn on(access: &Access, deployment: &str) -> Vec<P> {
        DEPLOYMENT.into_iter().filter(|p| access.can(deployment, *p)).collect()
    }

    fn server(access: &Access) -> Vec<P> {
        SERVER.into_iter().filter(|p| access.has(*p)).collect()
    }

    #[tokio::test]
    async fn builtin_roles_match_the_old_fixed_roles() {
        let db = test_pool().await;
        let owner = create_user(&db, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let admin = create_user(&db, "adm", "adm-password-1", Role::admin()).await.unwrap();
        let dev = create_user(&db, "dev", "dev-password-1", Role::deployer()).await.unwrap();
        let viewer = create_user(&db, "eye", "eye-password-1", Role::viewer()).await.unwrap();
        let mine = create(&db, "mine", &spec(), &dev).await.unwrap();
        let theirs = create(&db, "theirs", &spec(), &owner).await.unwrap();

        let operate = vec![P::DepView, P::DepDeploy, P::DepEdit, P::DepSecrets, P::DepShell, P::DepDomains, P::DepCi];
        let everything = DEPLOYMENT.to_vec();
        let load = |u: &User| {
            let db = db.clone();
            let u = u.clone();
            async move { Access::load(&db, &u).await.unwrap() }
        };

        let a = load(&owner).await;
        assert_eq!((on(&a, &mine.id), on(&a, &theirs.id), server(&a)), (everything.clone(), everything.clone(), SERVER.to_vec()));

        let a = load(&admin).await;
        let admin_server = vec![P::DepCreate, P::Volumes, P::Proxy, P::Settings, P::Import];
        assert_eq!((on(&a, &mine.id), on(&a, &theirs.id), server(&a)), (everything.clone(), everything, admin_server));
        assert!(a.everywhere(P::DepShell), "admins reach containers outside any deployment");

        let a = load(&dev).await;
        assert_eq!((on(&a, &mine.id), on(&a, &theirs.id), server(&a)), (operate, vec![P::DepView], vec![P::DepCreate]));
        assert!(a.has(P::SecretUse) && !a.has(P::SecretManage), "deployers use saved secrets but don't manage them");
        assert!(!a.everywhere(P::DepShell));

        let a = load(&viewer).await;
        assert_eq!((on(&a, &mine.id), on(&a, &theirs.id), server(&a)), (vec![P::DepView], vec![P::DepView], vec![]));
    }

    async fn custom_role(db: &SqlitePool, name: &str, permissions: &str, scope: &str) {
        sqlx::query("INSERT INTO roles (name, permissions, scope) VALUES (?, ?, ?)")
            .bind(name)
            .bind(permissions)
            .bind(scope)
            .execute(db)
            .await
            .unwrap();
    }

    async fn grant(db: &SqlitePool, deployment: &str, user: &User, role: &str) {
        sqlx::query("INSERT INTO deployment_grants (deployment_id, user_id, role) VALUES (?, ?, ?)")
            .bind(deployment)
            .bind(&user.id)
            .bind(role)
            .execute(db)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn custom_role_only_reaches_its_scope() {
        let db = test_pool().await;
        let owner = create_user(&db, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let a = create(&db, "a", &spec(), &owner).await.unwrap();
        let b = create(&db, "b", &spec(), &owner).await.unwrap();
        custom_role(&db, "frontend", r#"["dep.view","dep.deploy","dep.create"]"#, &format!(r#"["{}"]"#, a.id)).await;
        let fe = create_user(&db, "fe", "fe-password-12", Role("frontend".into())).await.unwrap();

        let access = Access::load(&db, &fe).await.unwrap();
        assert_eq!(on(&access, &a.id), [P::DepView, P::DepDeploy]);
        assert!(on(&access, &b.id).is_empty(), "outside the scope there is no access at all");

        let made = create(&db, "c", &spec(), &fe).await.unwrap();
        let access = Access::load(&db, &fe).await.unwrap();
        assert_eq!(on(&access, &made.id), [P::DepView, P::DepDeploy], "a new deployment outside the scope gets a grant");
    }

    #[tokio::test]
    async fn grant_overrides_the_role_but_not_for_an_owner() {
        let db = test_pool().await;
        let owner = create_user(&db, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let admin = create_user(&db, "adm", "adm-password-1", Role::admin()).await.unwrap();
        let viewer = create_user(&db, "eye", "eye-password-1", Role::viewer()).await.unwrap();
        let d = create(&db, "shop", &spec(), &owner).await.unwrap();
        grant(&db, &d.id, &admin, "viewer").await;
        grant(&db, &d.id, &viewer, "deployer").await;
        grant(&db, &d.id, &owner, "viewer").await;

        assert_eq!(on(&Access::load(&db, &admin).await.unwrap(), &d.id), [P::DepView]);
        let viewer_access = Access::load(&db, &viewer).await.unwrap();
        assert!(viewer_access.can(&d.id, P::DepDeploy) && viewer_access.can(&d.id, P::DepSecrets));
        assert!(!viewer_access.has(P::DepCreate), "a grant only changes that deployment");
        assert_eq!(on(&Access::load(&db, &owner).await.unwrap(), &d.id), DEPLOYMENT);
    }

    #[tokio::test]
    async fn deploys_announce_their_status_only_to_users_who_can_see_them() {
        use crate::events::Event;
        let db = test_pool().await;
        sqlx::query("INSERT INTO roles (name, permissions, scope) VALUES ('elsewhere', '[\"dep.view\"]', '[\"another-deployment\"]')")
            .execute(&db)
            .await
            .unwrap();
        let owner = create_user(&db, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let dev = create_user(&db, "dev", "dev-password-1", Role("elsewhere".into())).await.unwrap();
        let d = create(&db, "shop", &spec(), &owner).await.unwrap();
        let mut events = crate::events::subscribe();
        begin_deploy(&db, &d.id, &history::Started::user(&owner), &history::DeploySource::default()).await.unwrap();
        finish(&db, &d.id, "running", "ok").await.unwrap();

        let heard: Vec<Event> = std::iter::from_fn(|| events.try_recv().ok())
            .filter(|e| matches!(e, Event::DeploymentStatus { id, .. } if id == &d.id))
            .collect();
        let status = |status: &str| Event::DeploymentStatus { id: d.id.clone(), status: status.into(), revision: 1 };
        assert_eq!(heard, [status("deploying"), status("running")]);
        let owner_access = Access::load(&db, &owner).await.unwrap();
        let dev_access = Access::load(&db, &dev).await.unwrap();
        assert!(heard.iter().all(|e| e.visible_to(&owner_access) && !e.visible_to(&dev_access)));
    }

    #[tokio::test]
    async fn only_one_deploy_at_a_time() {
        let db = test_pool().await;
        let owner = create_user(&db, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let d = create(&db, "shop", &spec(), &owner).await.unwrap();
        let source = history::DeploySource::default();
        let started = history::Started::user(&owner);
        let revision = |b: Option<Begun>| b.map(|b| b.revision);
        assert_eq!(revision(begin_deploy(&db, &d.id, &started, &source).await.unwrap()), Some(1));
        assert_eq!(begin_deploy(&db, &d.id, &started, &source).await.unwrap(), None);
        finish(&db, &d.id, "running", "ok").await.unwrap();
        assert_eq!(revision(begin_deploy(&db, &d.id, &started, &source).await.unwrap()), Some(2));
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM deploys").fetch_one(&db).await.unwrap();
        assert_eq!(rows, 2, "a refused deploy leaves no history row");
    }
}
