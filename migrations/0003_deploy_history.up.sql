-- One row per deploy. trigger: ui, deploy_token or webhook. user_id or token_id: who started it, while they exist.
-- triggered_by: their name when the deploy started, or the webhook sender. source: JSON of what was deployed.
-- steps: JSON list of {step, detail, started_ms, finished_ms, failed}.
CREATE TABLE deploys (
    id TEXT PRIMARY KEY,
    deployment_id TEXT NOT NULL REFERENCES deployments(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('deploying', 'running', 'failed', 'cancelled')),
    "trigger" TEXT NOT NULL CHECK ("trigger" IN ('ui', 'deploy_token', 'webhook')),
    user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
    token_id TEXT REFERENCES deploy_tokens(id) ON DELETE SET NULL,
    triggered_by TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT '{}',
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    output TEXT NOT NULL DEFAULT '',
    steps TEXT NOT NULL DEFAULT '[]'
);

CREATE INDEX deploys_by_deployment ON deploys (deployment_id, started_at);
