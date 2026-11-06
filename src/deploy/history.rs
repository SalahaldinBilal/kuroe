use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

use super::spec::{Source, Spec};
use crate::auth::User;
use crate::db::now;
use crate::deploy::tokens::Grant;

pub const DEFAULT_KEEP: i64 = 50;
const RESTARTED: &str = "kuroe restarted during this deploy";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Ui,
    DeployToken,
    Webhook,
}

impl Trigger {
    fn as_str(self) -> &'static str {
        match self {
            Trigger::Ui => "ui",
            Trigger::DeployToken => "deploy_token",
            Trigger::Webhook => "webhook",
        }
    }

    fn parse(text: &str) -> anyhow::Result<Self> {
        [Trigger::Ui, Trigger::DeployToken, Trigger::Webhook]
            .into_iter()
            .find(|t| t.as_str() == text)
            .ok_or_else(|| anyhow::anyhow!("unknown deploy trigger {text}"))
    }
}

/// Who or what started a deploy. `name` is kept as it was, so the history still says who after they're deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub trigger: Trigger,
    pub user_id: Option<String>,
    pub token_id: Option<String>,
    pub name: String,
}

impl Started {
    pub fn user(user: &User) -> Self {
        Started { trigger: Trigger::Ui, user_id: Some(user.id.clone()), token_id: None, name: user.username.clone() }
    }

    pub fn token(grant: &Grant) -> Self {
        Started { trigger: Trigger::DeployToken, user_id: None, token_id: Some(grant.token_id.clone()), name: grant.name.clone() }
    }

    pub fn webhook(sender: impl Into<String>) -> Self {
        Started { trigger: Trigger::Webhook, user_id: None, token_id: None, name: sender.into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    ResolvingSecrets,
    Cloning,
    CheckingPool,
    Building,
    Pulling,
    Starting,
    HealthCheck,
    SwitchingTraffic,
    Draining,
    StoppingOld,
    PublishingRoutes,
    InstallingFiles,
    RestoringRoutes,
    StoppingNew,
    CleaningUp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    pub step: Step,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// None while the step is still to come.
    #[serde(default)]
    pub started_ms: Option<i64>,
    #[serde(default)]
    pub finished_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
    /// Index of the first output line written during this step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_line: Option<usize>,
}

impl StepRecord {
    fn waiting(step: Step, detail: Option<String>) -> Self {
        StepRecord { step, detail, started_ms: None, finished_ms: None, failed: false, first_line: None }
    }

    fn running(&self) -> bool {
        self.started_ms.is_some() && self.finished_ms.is_none()
    }
}

/// A step expected later in the deploy, with what it works on when that's known.
pub type Planned = (Step, Option<String>);

/// Replaces the steps still to come with `upcoming`.
pub fn plan_steps(steps: &mut Vec<StepRecord>, upcoming: &[Planned]) {
    steps.retain(|s| s.started_ms.is_some());
    steps.extend(upcoming.iter().map(|(step, detail)| StepRecord::waiting(*step, detail.clone())));
}

/// Ends the running step and starts `step`. A planned step it skipped over didn't happen, so it goes.
pub fn start_step(steps: &mut Vec<StepRecord>, step: Step, detail: Option<String>, at: i64) {
    close_running(steps, at);
    let first_waiting = steps.iter().position(|s| s.started_ms.is_none()).unwrap_or(steps.len());
    let found = steps[first_waiting..]
        .iter()
        .position(|s| s.step == step && (s.detail.is_none() || s.detail == detail))
        .map(|i| first_waiting + i);
    match found {
        Some(index) => drop(steps.drain(first_waiting..index)),
        None => steps.insert(first_waiting, StepRecord::waiting(step, None)),
    }
    let entry = &mut steps[first_waiting];
    entry.detail = detail;
    entry.started_ms = Some(at);
}

pub fn mark_first_line(steps: &mut [StepRecord], line: usize) {
    if let Some(step) = steps.iter_mut().find(|s| s.running()) {
        step.first_line = Some(line);
    }
}

/// Marks the step that was running when the deploy went wrong. Steps after it, like a rollback, keep their marks.
pub fn fail_step(steps: &mut [StepRecord]) {
    if steps.iter().any(|s| s.failed) {
        return;
    }
    if let Some(step) = steps.iter_mut().rev().find(|s| s.started_ms.is_some()) {
        step.failed = true;
    }
}

/// Ends the running step and drops the ones the deploy never reached.
pub fn finish_steps(steps: &mut Vec<StepRecord>, at: i64) {
    close_running(steps, at);
    steps.retain(|s| s.started_ms.is_some());
}

pub fn current_step(steps: &[StepRecord]) -> Option<&StepRecord> {
    steps.iter().find(|s| s.running())
}

fn close_running(steps: &mut [StepRecord], at: i64) {
    for step in steps.iter_mut().filter(|s| s.running()) {
        step.finished_ms = Some(at);
    }
}

pub fn now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

/// What a deploy deployed. Filled from the spec when it starts, then with the commit or digest once known.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploySource {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// github, gitlab or gitea, when kuroe can tell, for links to the commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

impl DeploySource {
    pub fn of(spec: &Spec) -> Self {
        let kind = match &spec.source {
            Source::Image { .. } => "image",
            Source::Git { .. } => "git",
            Source::Compose { .. } => "compose",
            Source::Dockerfile { .. } => "dockerfile",
            Source::Upload { .. } => "upload",
            Source::Static {} => "static",
        };
        let mut source = DeploySource { kind: kind.to_owned(), ..Default::default() };
        match &spec.source {
            Source::Git { url, reference, .. } => {
                source.url = Some(without_login(url));
                source.reference.clone_from(reference);
            }
            Source::Image { image, .. } => source.image = Some(image.clone()),
            _ => {}
        }
        source
    }
}

/// Drops a `user:password@` someone typed into a clone URL.
pub fn without_login(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_owned() };
    let end = rest.find('/').unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
        None => url.to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub id: String,
    pub revision: i64,
    pub status: String,
    pub trigger: Trigger,
    /// The user's current name while they exist, else the name saved when the deploy started.
    pub triggered_by: String,
    pub user_id: Option<String>,
    pub token_id: Option<String>,
    pub source: DeploySource,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Detail {
    #[serde(flatten)]
    pub summary: Summary,
    pub output: String,
    pub steps: Vec<StepRecord>,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    revision: i64,
    status: String,
    trigger: String,
    triggered_by: String,
    user_id: Option<String>,
    token_id: Option<String>,
    source: String,
    started_at: i64,
    finished_at: Option<i64>,
}

impl TryFrom<Row> for Summary {
    type Error = anyhow::Error;

    fn try_from(row: Row) -> anyhow::Result<Self> {
        Ok(Summary {
            trigger: Trigger::parse(&row.trigger)?,
            source: serde_json::from_str(&row.source)?,
            id: row.id,
            revision: row.revision,
            status: row.status,
            triggered_by: row.triggered_by,
            user_id: row.user_id,
            token_id: row.token_id,
            started_at: row.started_at,
            finished_at: row.finished_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct DetailRow {
    #[sqlx(flatten)]
    row: Row,
    output: String,
    steps: String,
}

pub async fn insert(
    conn: &mut SqliteConnection,
    deployment_id: &str,
    revision: i64,
    started: &Started,
    source: &DeploySource,
) -> anyhow::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        r#"INSERT INTO deploys (id, deployment_id, revision, status, "trigger", user_id, token_id, triggered_by, source, started_at)
           VALUES (?, ?, ?, 'deploying', ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(&id)
    .bind(deployment_id)
    .bind(revision)
    .bind(started.trigger.as_str())
    .bind(&started.user_id)
    .bind(&started.token_id)
    .bind(&started.name)
    .bind(serde_json::to_string(source)?)
    .bind(now())
    .execute(conn)
    .await?;
    Ok(id)
}

pub async fn progress(db: &SqlitePool, id: &str, output: &str, steps: &[StepRecord]) -> anyhow::Result<()> {
    sqlx::query("UPDATE deploys SET output = ?, steps = ? WHERE id = ? AND status = 'deploying'")
        .bind(output)
        .bind(serde_json::to_string(steps)?)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_source(db: &SqlitePool, id: &str, source: &DeploySource) -> anyhow::Result<()> {
    sqlx::query("UPDATE deploys SET source = ? WHERE id = ?").bind(serde_json::to_string(source)?).bind(id).execute(db).await?;
    Ok(())
}

/// `status` is running, failed or cancelled.
pub async fn close(db: &SqlitePool, id: &str, status: &str, output: &str, steps: &[StepRecord]) -> anyhow::Result<()> {
    sqlx::query("UPDATE deploys SET status = ?, output = ?, steps = ?, finished_at = ? WHERE id = ?")
        .bind(status)
        .bind(output)
        .bind(serde_json::to_string(steps)?)
        .bind(now())
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Newest first, without output.
pub async fn list(db: &SqlitePool, deployment_id: &str, offset: i64, limit: i64) -> anyhow::Result<Vec<Summary>> {
    let rows: Vec<Row> = sqlx::query_as(
        r#"SELECT d.id, d.revision, d.status, d."trigger", COALESCE(u.username, d.triggered_by) AS triggered_by,
                  d.user_id, d.token_id, d.source, d.started_at, d.finished_at
           FROM deploys d LEFT JOIN users u ON u.id = d.user_id
           WHERE d.deployment_id = ? ORDER BY d.started_at DESC, d.rowid DESC LIMIT ? OFFSET ?"#,
    )
    .bind(deployment_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await?;
    rows.into_iter().map(Summary::try_from).collect()
}

pub async fn get(db: &SqlitePool, deployment_id: &str, id: &str) -> anyhow::Result<Option<Detail>> {
    let row: Option<DetailRow> = sqlx::query_as(
        r#"SELECT d.id, d.revision, d.status, d."trigger", COALESCE(u.username, d.triggered_by) AS triggered_by,
                  d.user_id, d.token_id, d.source, d.started_at, d.finished_at, d.output, d.steps
           FROM deploys d LEFT JOIN users u ON u.id = d.user_id
           WHERE d.id = ? AND d.deployment_id = ?"#,
    )
    .bind(id)
    .bind(deployment_id)
    .fetch_optional(db)
    .await?;
    let Some(DetailRow { row, output, steps }) = row else { return Ok(None) };
    Ok(Some(Detail { summary: row.try_into()?, output, steps: serde_json::from_str(&steps)? }))
}

pub async fn latest(db: &SqlitePool, deployment_id: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT id FROM deploys WHERE deployment_id = ? ORDER BY started_at DESC, rowid DESC LIMIT 1")
        .bind(deployment_id)
        .fetch_optional(db)
        .await
}

/// Deploys that were running when kuroe stopped can't finish, so they and their deployments count as failed.
pub async fn fail_interrupted(db: &SqlitePool) -> sqlx::Result<u64> {
    let mut tx = db.begin().await?;
    let failed = sqlx::query(
        "UPDATE deploys SET status = 'failed', finished_at = ?,
             output = output || CASE WHEN output = '' THEN '' ELSE char(10) END || ?
         WHERE status = 'deploying'",
    )
    .bind(now())
    .bind(RESTARTED)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query(
        "UPDATE deployments SET status = 'failed', updated_at = ?,
             last_output = COALESCE(
                 (SELECT output FROM deploys WHERE deploys.deployment_id = deployments.id ORDER BY started_at DESC, rowid DESC LIMIT 1),
                 ?)
         WHERE status = 'deploying'",
    )
    .bind(now())
    .bind(RESTARTED)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(failed)
}

/// Keeps the newest `keep` finished deploys of each deployment.
pub async fn prune(db: &SqlitePool, keep: i64) -> sqlx::Result<u64> {
    Ok(sqlx::query(
        "DELETE FROM deploys WHERE id IN (
             SELECT id FROM (
                 SELECT id, ROW_NUMBER() OVER (PARTITION BY deployment_id ORDER BY started_at DESC, rowid DESC) AS n
                 FROM deploys WHERE status != 'deploying'
             ) WHERE n > ?
         )",
    )
    .bind(keep)
    .execute(db)
    .await?
    .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{Role, create_user};
    use crate::db::test_pool;
    use crate::deploy::{self, begin_deploy, finish};

    fn git_spec() -> Spec {
        Spec::new(Source::Git {
            url: "https://bob:hunter2@git.example.com/acme/api.git".into(),
            reference: Some("main".into()),
            context: None,
            dockerfile: None,
            token: Some("ghp_secret".into()),
            identity: None,
            compose_file: None,
        })
    }

    fn names(steps: &[StepRecord]) -> Vec<(Step, bool)> {
        steps.iter().map(|s| (s.step, s.started_ms.is_some())).collect()
    }

    #[test]
    fn planned_steps_are_ticked_off_in_order() {
        let mut steps = Vec::new();
        start_step(&mut steps, Step::ResolvingSecrets, None, 1);
        plan_steps(&mut steps, &[(Step::Cloning, None), (Step::Building, Some("web".into())), (Step::Starting, None)]);
        assert_eq!(names(&steps), [(Step::ResolvingSecrets, true), (Step::Cloning, false), (Step::Building, false), (Step::Starting, false)]);

        start_step(&mut steps, Step::Cloning, None, 2);
        assert_eq!(steps[0].finished_ms, Some(2), "starting a step ends the one before");
        start_step(&mut steps, Step::Starting, Some("shop-blue".into()), 3);
        assert_eq!(names(&steps), [(Step::ResolvingSecrets, true), (Step::Cloning, true), (Step::Starting, true)], "a skipped step goes");
        assert_eq!(steps[2].detail.as_deref(), Some("shop-blue"));

        start_step(&mut steps, Step::StoppingOld, Some("shop-green".into()), 4);
        assert_eq!(names(&steps).last(), Some(&(Step::StoppingOld, true)), "an unplanned step is added where it happens");
    }

    #[test]
    fn steps_saved_before_line_marks_still_load() {
        let saved = r#"[{"step":"cloning","started_ms":1,"finished_ms":2},{"step":"building","detail":"web","started_ms":2,"finished_ms":9,"failed":true}]"#;
        let steps: Vec<StepRecord> = serde_json::from_str(saved).unwrap();
        assert_eq!(steps.len(), 2);
        assert!(steps.iter().all(|s| s.first_line.is_none()));
    }

    #[test]
    fn a_new_plan_replaces_only_what_is_still_to_come() {
        let mut steps = Vec::new();
        start_step(&mut steps, Step::ResolvingSecrets, None, 1);
        plan_steps(&mut steps, &[(Step::Cloning, None), (Step::Starting, None)]);
        start_step(&mut steps, Step::Cloning, None, 2);
        plan_steps(&mut steps, &[(Step::Building, Some("api".into())), (Step::Building, Some("worker".into())), (Step::Starting, None)]);
        assert_eq!(steps.len(), 5);
        start_step(&mut steps, Step::Building, Some("worker".into()), 3);
        assert_eq!(steps[2].detail.as_deref(), Some("worker"), "a planned detail must match");
        assert_eq!(steps.len(), 4);
    }

    #[test]
    fn the_failed_step_is_the_one_running_when_it_went_wrong() {
        let mut steps = Vec::new();
        start_step(&mut steps, Step::Starting, None, 1);
        start_step(&mut steps, Step::HealthCheck, None, 2);
        fail_step(&mut steps);
        start_step(&mut steps, Step::StoppingNew, None, 3);
        fail_step(&mut steps);
        plan_steps(&mut steps, &[(Step::SwitchingTraffic, None)]);
        finish_steps(&mut steps, 4);
        let failed: Vec<Step> = steps.iter().filter(|s| s.failed).map(|s| s.step).collect();
        assert_eq!(failed, [Step::HealthCheck]);
        assert_eq!(names(&steps).len(), 3, "steps never reached are dropped at the end");
        assert!(steps.iter().all(|s| s.finished_ms.is_some()));
    }

    #[test]
    fn the_source_never_carries_a_login() {
        let source = DeploySource::of(&git_spec());
        assert_eq!(source.url.as_deref(), Some("https://git.example.com/acme/api.git"));
        assert_eq!((source.kind.as_str(), source.reference.as_deref()), ("git", Some("main")));
        let json = serde_json::to_string(&source).unwrap();
        assert!(!json.contains("hunter2") && !json.contains("ghp_secret"), "{json}");
        assert_eq!(without_login("https://github.com/a/b@c.git"), "https://github.com/a/b@c.git");
        assert_eq!(without_login("git@github.com:a/b.git"), "git@github.com:a/b.git");
    }

    #[tokio::test]
    async fn a_deploy_is_recorded_from_start_to_finish() {
        let db = test_pool().await;
        let owner = create_user(&db, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let d = deploy::create(&db, "api", &git_spec(), &owner).await.unwrap();
        let begun = begin_deploy(&db, &d.id, &Started::user(&owner), &DeploySource::of(&d.spec)).await.unwrap().unwrap();
        assert_eq!(begun.revision, 1);

        let mut steps = Vec::new();
        start_step(&mut steps, Step::Cloning, None, 1);
        progress(&db, &begun.deploy, "cloning", &steps).await.unwrap();
        let live = get(&db, &d.id, &begun.deploy).await.unwrap().unwrap();
        assert_eq!((live.summary.status.as_str(), live.output.as_str()), ("deploying", "cloning"));

        finish_steps(&mut steps, 2);
        close(&db, &begun.deploy, "running", "cloning\ndone", &steps).await.unwrap();
        finish(&db, &d.id, "running", "cloning\ndone").await.unwrap();
        let done = get(&db, &d.id, &begun.deploy).await.unwrap().unwrap();
        assert_eq!(done.summary.status, "running");
        assert_eq!(done.summary.trigger, Trigger::Ui);
        assert_eq!(done.summary.triggered_by, "boss");
        assert_eq!(done.summary.user_id.as_deref(), Some(owner.id.as_str()));
        assert_eq!(done.steps, steps);
        assert!(done.summary.finished_at.is_some());
        assert!(get(&db, "another-deployment", &begun.deploy).await.unwrap().is_none(), "only through its own deployment");

        let listed = list(&db, &d.id, 0, 10).await.unwrap();
        assert_eq!(listed, [done.summary]);
    }

    #[tokio::test]
    async fn the_history_keeps_who_after_they_are_deleted() {
        let db = test_pool().await;
        let owner = create_user(&db, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let dev = create_user(&db, "dev", "dev-password-1", Role::deployer()).await.unwrap();
        let d = deploy::create(&db, "api", &git_spec(), &owner).await.unwrap();
        let begun = begin_deploy(&db, &d.id, &Started::user(&dev), &DeploySource::default()).await.unwrap().unwrap();
        sqlx::query("DELETE FROM users WHERE id = ?").bind(&dev.id).execute(&db).await.unwrap();
        let row = get(&db, &d.id, &begun.deploy).await.unwrap().unwrap();
        assert_eq!((row.summary.user_id, row.summary.triggered_by.as_str()), (None, "dev"));
    }

    #[tokio::test]
    async fn a_restart_fails_what_was_deploying() {
        let db = test_pool().await;
        let owner = create_user(&db, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let d = deploy::create(&db, "api", &git_spec(), &owner).await.unwrap();
        let begun = begin_deploy(&db, &d.id, &Started::user(&owner), &DeploySource::default()).await.unwrap().unwrap();
        progress(&db, &begun.deploy, "building web", &[]).await.unwrap();

        assert_eq!(fail_interrupted(&db).await.unwrap(), 1);
        let row = get(&db, &d.id, &begun.deploy).await.unwrap().unwrap();
        assert_eq!(row.summary.status, "failed");
        assert_eq!(row.output, format!("building web\n{RESTARTED}"));
        let after = deploy::get(&db, &d.id).await.unwrap().unwrap();
        assert_eq!((after.status.as_str(), after.last_output.as_str()), ("failed", row.output.as_str()));
        assert!(begin_deploy(&db, &d.id, &Started::user(&owner), &DeploySource::default()).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn pruning_keeps_the_newest_of_each_deployment() {
        let db = test_pool().await;
        let owner = create_user(&db, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let a = deploy::create(&db, "a", &git_spec(), &owner).await.unwrap();
        let b = deploy::create(&db, "b", &git_spec(), &owner).await.unwrap();
        for d in [&a, &a, &a, &b] {
            let begun = begin_deploy(&db, &d.id, &Started::webhook("GitHub push"), &DeploySource::default()).await.unwrap().unwrap();
            close(&db, &begun.deploy, "running", "", &[]).await.unwrap();
            finish(&db, &d.id, "running", "").await.unwrap();
        }
        assert_eq!(prune(&db, 2).await.unwrap(), 1);
        let revisions: Vec<i64> = list(&db, &a.id, 0, 10).await.unwrap().iter().map(|s| s.revision).collect();
        assert_eq!(revisions, [3, 2]);
        assert_eq!(list(&db, &b.id, 0, 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn history_goes_with_its_deployment() {
        let db = test_pool().await;
        let owner = create_user(&db, "boss", "boss-password-1", Role::owner()).await.unwrap();
        let d = deploy::create(&db, "api", &git_spec(), &owner).await.unwrap();
        begin_deploy(&db, &d.id, &Started::user(&owner), &DeploySource::default()).await.unwrap();
        deploy::delete(&db, &d.id).await.unwrap();
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM deploys").fetch_one(&db).await.unwrap();
        assert_eq!(left, 0);
    }
}
