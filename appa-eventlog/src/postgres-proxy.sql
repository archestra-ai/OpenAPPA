-- Apply through the embedding host's migration system before enabling the
-- authenticated proxy against a PostgreSQL LogStore. This leaves the existing
-- openappa_events and openappa_policy_files tables unchanged.
CREATE TABLE proxy_events (
    root_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    body_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'completed')),
    boot_owner TEXT NOT NULL,
    response BYTEA,
    PRIMARY KEY (root_id, event_id)
);
CREATE TABLE proxy_offer_bindings (
    offer_id TEXT PRIMARY KEY,
    root_id TEXT NOT NULL,
    tool TEXT NOT NULL,
    arguments_sha256 TEXT NOT NULL,
    kind TEXT NOT NULL,
    deployment_fingerprint TEXT NOT NULL,
    batch_id TEXT,
    position INTEGER
);
CREATE TABLE proxy_approval_grants (
    approval_id TEXT PRIMARY KEY,
    root_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    body_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'completed'))
);
CREATE TABLE proxy_dispatch_bindings (
    root_id TEXT NOT NULL,
    lane_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    tool TEXT NOT NULL,
    arguments_sha256 TEXT NOT NULL,
    dispatch TEXT NOT NULL,
    spawn_binding TEXT,
    deployment_fingerprint TEXT NOT NULL,
    batch_id TEXT,
    position INTEGER,
    PRIMARY KEY (root_id, lane_id, call_id)
);
CREATE TABLE proxy_batches (
    batch_id TEXT PRIMARY KEY,
    root_id TEXT NOT NULL,
    lane_id TEXT NOT NULL,
    core_batch_id TEXT NOT NULL,
    positions INTEGER NOT NULL,
    basis BIGINT NOT NULL,
    deployment_fingerprint TEXT NOT NULL
);
CREATE TABLE proxy_batch_positions (
    batch_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    call_id TEXT NOT NULL,
    tool TEXT NOT NULL,
    arguments_sha256 TEXT NOT NULL,
    arguments TEXT NOT NULL,
    effective_tool TEXT NOT NULL,
    effective_arguments_sha256 TEXT NOT NULL,
    effective_arguments TEXT NOT NULL,
    dispatch TEXT,
    spawn BOOLEAN NOT NULL,
    spawn_binding TEXT,
    authorized BOOLEAN NOT NULL,
    PRIMARY KEY (batch_id, position),
    UNIQUE (batch_id, call_id)
);
CREATE TABLE proxy_batch_quarantines (
    batch_id TEXT PRIMARY KEY,
    reason TEXT NOT NULL
);
CREATE TABLE checkpoints (
    id TEXT PRIMARY KEY,
    source_root TEXT NOT NULL,
    basis BIGINT NOT NULL,
    policy_key TEXT NOT NULL,
    snapshot BYTEA NOT NULL
);
