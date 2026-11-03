-- The size of each kuroe volume at the last storage measurement, by Docker volume name. project is the Compose project or Swarm stack of a deployment's own volume, empty for a shared volume.
CREATE TABLE volume_sizes (
    name TEXT PRIMARY KEY,
    project TEXT NOT NULL DEFAULT '',
    bytes INTEGER NOT NULL,
    measured_at INTEGER NOT NULL
);

-- A soft limit on a volume, checked against its measured size. stop: stop the deployments that use the volume while it's over.
CREATE TABLE volume_limits (
    name TEXT PRIMARY KEY,
    max_bytes INTEGER NOT NULL CHECK (max_bytes > 0),
    stop INTEGER NOT NULL DEFAULT 0,
    set_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    set_at INTEGER NOT NULL
);
