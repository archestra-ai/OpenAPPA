-- The tables an embedding host's migrations install. The library never creates
-- them; this copy exists so the ignored PostgreSQL tests have a database to run on.

CREATE TABLE openappa_events (
    root text NOT NULL,
    seq bigint NOT NULL,
    payload bytea NOT NULL,
    CONSTRAINT openappa_events_root_seq_pk PRIMARY KEY (root, seq),
    CONSTRAINT openappa_events_seq_nonnegative CHECK (seq >= 0)
);

CREATE TABLE openappa_file_events (
    workspace text NOT NULL,
    seq bigint NOT NULL,
    payload bytea NOT NULL,
    CONSTRAINT openappa_file_events_workspace_seq_pk PRIMARY KEY (workspace, seq),
    CONSTRAINT openappa_file_events_seq_nonnegative CHECK (seq >= 0)
);

CREATE TABLE openappa_file_roots (
    root text PRIMARY KEY,
    workspace text NOT NULL,
    seq bigint NOT NULL,
    CONSTRAINT openappa_file_roots_seq_nonnegative CHECK (seq >= 0)
);
CREATE INDEX openappa_file_roots_workspace_idx ON openappa_file_roots (workspace);

CREATE TABLE openappa_host_keys (
    key text NOT NULL,
    root text NOT NULL,
    CONSTRAINT openappa_host_keys_key_root_pk PRIMARY KEY (key, root)
);
CREATE INDEX openappa_host_keys_root_idx ON openappa_host_keys (root);

CREATE TABLE openappa_policy_files (
    hash text PRIMARY KEY,
    bytes bytea NOT NULL
);

CREATE TABLE openappa_sessions (
    actor text PRIMARY KEY,
    root text NOT NULL,
    organization_id text NOT NULL,
    caller_id text,
    session_id text NOT NULL,
    parent_id text,
    start_decision jsonb NOT NULL
);
CREATE INDEX openappa_sessions_root_idx ON openappa_sessions (root);

CREATE TABLE openappa_operations (
    organization_id text NOT NULL,
    caller_id text,
    session_id text NOT NULL,
    operation_id text NOT NULL,
    root text NOT NULL,
    status text NOT NULL,
    input jsonb NOT NULL,
    decision jsonb,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT openappa_operations_pk PRIMARY KEY (organization_id, session_id, operation_id),
    CONSTRAINT openappa_operations_status CHECK (
        (status = 'pending' AND decision IS NULL)
        OR (status = 'complete' AND decision IS NOT NULL)
    )
);
CREATE INDEX openappa_operations_pending_idx ON openappa_operations (root) WHERE status = 'pending';

CREATE TABLE openappa_processed_results (
    organization_id text NOT NULL,
    caller_id text,
    session_id text NOT NULL,
    tool_call_id text NOT NULL,
    root text NOT NULL,
    status text NOT NULL,
    approved_output text,
    decision jsonb,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT openappa_results_pk PRIMARY KEY (organization_id, session_id, tool_call_id),
    CONSTRAINT openappa_results_status CHECK (
        (status = 'pending' AND approved_output IS NULL AND decision IS NULL)
        OR (status = 'complete' AND approved_output IS NOT NULL AND decision IS NOT NULL)
    )
);
CREATE INDEX openappa_results_pending_idx ON openappa_processed_results (root) WHERE status = 'pending';

CREATE TABLE openappa_held_peer_messages (
    seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    id text NOT NULL UNIQUE,
    receiver text NOT NULL,
    digest text NOT NULL,
    label jsonb NOT NULL,
    body text NOT NULL,
    expires_at bigint NOT NULL,
    notified boolean NOT NULL DEFAULT false
);
CREATE INDEX openappa_held_peer_messages_receiver_idx ON openappa_held_peer_messages (receiver, seq);

CREATE TABLE openappa_embedded_peer_messages (
    seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    id text NOT NULL UNIQUE,
    root text NOT NULL,
    sender text NOT NULL,
    recipient text NOT NULL,
    pending_spawn text,
    dispatch text NOT NULL,
    digest text NOT NULL,
    label jsonb NOT NULL,
    body text,
    status text NOT NULL,
    read_call_id text,
    read_arguments text,
    decision jsonb,
    expires_at bigint NOT NULL,
    created_at bigint NOT NULL,
    CONSTRAINT openappa_embedded_peer_status CHECK (status IN ('held', 'direct', 'read')),
    CONSTRAINT openappa_embedded_peer_dispatch_uidx UNIQUE (root, sender, dispatch)
);
CREATE INDEX openappa_embedded_peer_recipient_idx ON openappa_embedded_peer_messages (root, recipient, status);
CREATE UNIQUE INDEX openappa_embedded_peer_read_call_uidx ON openappa_embedded_peer_messages (root, recipient, read_call_id) WHERE read_call_id IS NOT NULL;
