CREATE TABLE users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    host_mounts INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE sessions (
    token TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE INDEX sessions_user_id ON sessions(user_id);

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE roles (
    name TEXT PRIMARY KEY,
    builtin INTEGER NOT NULL DEFAULT 0,
    description TEXT NOT NULL DEFAULT '',
    permissions TEXT NOT NULL DEFAULT '[]',
    scope TEXT NOT NULL DEFAULT 'all'
);

INSERT INTO roles (name, builtin, description, permissions, scope) VALUES
    ('owner', 1, 'Everything, including users and updates.',
     '["dep.view","dep.create","dep.deploy","dep.edit","dep.secrets","dep.shell","dep.domains","dep.ci","dep.delete","vol","proxy","ident.use","ident.manage","settings","import","users","runners","secret.use","secret.manage"]',
     'all'),
    ('admin', 1, 'All deployments, proxy, settings and import.',
     '["dep.view","dep.create","dep.deploy","dep.edit","dep.secrets","dep.shell","dep.domains","dep.ci","dep.delete","vol","proxy","ident.use","ident.manage","settings","import","runners","secret.use","secret.manage"]',
     'all'),
    ('deployer', 1, 'Creates deployments and runs the ones it''s granted.',
     '["dep.view","dep.create","dep.deploy","dep.edit","dep.secrets","dep.shell","dep.domains","dep.ci","ident.use","secret.use"]',
     'granted'),
    ('viewer', 1, 'Read only. Secrets stay hidden.', '["dep.view"]', 'all');

CREATE TABLE api_tokens (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    last_used_at INTEGER,
    UNIQUE (user_id, name)
);

CREATE TABLE deployments (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    spec TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('created', 'deploying', 'running', 'failed', 'stopped')),
    revision INTEGER NOT NULL DEFAULT 0,
    last_output TEXT NOT NULL DEFAULT '',
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    active_project TEXT,
    webhook_secret TEXT
);

CREATE TABLE deployment_grants (
    deployment_id TEXT NOT NULL REFERENCES deployments(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'deployer',
    PRIMARY KEY (deployment_id, user_id)
);

CREATE INDEX deployment_grants_user_id ON deployment_grants(user_id);

CREATE TABLE deploy_tokens (
    id TEXT PRIMARY KEY,
    deployment_id TEXT NOT NULL REFERENCES deployments(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('deploy')),
    token_hash TEXT NOT NULL UNIQUE,
    host_mounts INTEGER NOT NULL DEFAULT 0,
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER
);

CREATE INDEX deploy_tokens_deployment_id ON deploy_tokens(deployment_id);

CREATE TABLE volumes (
    name TEXT PRIMARY KEY,
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL
);

-- A redirect domain has no service and port 0. certificate: NULL picks a saved one by hostname, 'acme' forces Let's Encrypt, anything else is a certificate identity id.
CREATE TABLE domains (
    id TEXT PRIMARY KEY,
    deployment_id TEXT NOT NULL REFERENCES deployments(id) ON DELETE CASCADE,
    hostname TEXT NOT NULL UNIQUE COLLATE NOCASE,
    service TEXT NOT NULL,
    port INTEGER NOT NULL CHECK (port BETWEEN 0 AND 65535),
    tls INTEGER NOT NULL DEFAULT 1,
    probe_token TEXT NOT NULL,
    verified_at INTEGER,
    created_at INTEGER NOT NULL,
    certificate TEXT,
    options TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX domains_deployment_id ON domains(deployment_id);

CREATE TABLE identities (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    name TEXT NOT NULL UNIQUE,
    host TEXT NOT NULL DEFAULT '',
    username TEXT NOT NULL DEFAULT '',
    secret TEXT NOT NULL DEFAULT '',
    meta TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL,
    last_used_at INTEGER,
    expires_at INTEGER
);

CREATE TABLE runner_pools (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    provider TEXT NOT NULL,
    identity TEXT NOT NULL REFERENCES identities(id),
    scope_kind TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT '',
    labels TEXT NOT NULL DEFAULT '[]',
    min_idle INTEGER NOT NULL DEFAULT 1,
    max INTEGER NOT NULL DEFAULT 2,
    cpus REAL,
    memory_mb INTEGER,
    docker_mode TEXT NOT NULL DEFAULT 'dind',
    image TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    last_error TEXT,
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    net_in_mbit REAL,
    net_out_mbit REAL
);

CREATE TABLE saved_secrets (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    value TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX saved_secrets_fingerprint ON saved_secrets(fingerprint);

CREATE TABLE metric_series (
    id INTEGER PRIMARY KEY,
    deployment_id TEXT NOT NULL,
    service TEXT NOT NULL,
    container_id TEXT NOT NULL,
    UNIQUE (deployment_id, service, container_id)
);

CREATE TABLE container_metrics (
    series_id INTEGER NOT NULL REFERENCES metric_series(id),
    ts INTEGER NOT NULL,
    cpu_percent REAL NOT NULL,
    mem_bytes INTEGER NOT NULL,
    mem_limit INTEGER NOT NULL,
    net_rx INTEGER NOT NULL,
    net_tx INTEGER NOT NULL,
    blk_read INTEGER NOT NULL,
    blk_write INTEGER NOT NULL
);

CREATE INDEX container_metrics_series_ts ON container_metrics(series_id, ts);

CREATE TABLE host_metrics (
    ts INTEGER NOT NULL PRIMARY KEY,
    cpu_percent REAL NOT NULL,
    mem_used INTEGER NOT NULL,
    mem_total INTEGER NOT NULL,
    disk_used INTEGER NOT NULL,
    disk_total INTEGER NOT NULL,
    load1 REAL
);

CREATE TABLE storage_metrics (
    ts INTEGER NOT NULL,
    category TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    PRIMARY KEY (ts, category)
);

CREATE TABLE log_cursors (
    container_id TEXT PRIMARY KEY,
    last_ts TEXT NOT NULL
);
