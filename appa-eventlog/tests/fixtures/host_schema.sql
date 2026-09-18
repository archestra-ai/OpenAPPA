-- The tables an embedding host's migrations install. The library never creates
-- them; this copy exists so the ignored PostgreSQL tests have a database to run on.

CREATE TABLE openappa_events (
    root text NOT NULL,
    seq bigint NOT NULL,
    payload bytea NOT NULL,
    CONSTRAINT openappa_events_root_seq_pk PRIMARY KEY (root, seq),
    CONSTRAINT openappa_events_seq_nonnegative CHECK (seq >= 0)
);

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

CREATE TABLE openappa_offer_owners (
    organization_id text NOT NULL,
    caller_id text,
    session_id text NOT NULL,
    binding text NOT NULL,
    offer_id text NOT NULL,
    root text NOT NULL,
    parent_id text,
    arguments text,
    tool text,
    spelling text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT openappa_offer_owners_pk PRIMARY KEY (organization_id, offer_id)
);
CREATE INDEX openappa_offer_owners_created_at_idx ON openappa_offer_owners (created_at);
CREATE INDEX openappa_offer_owners_root_idx ON openappa_offer_owners (root);
CREATE INDEX openappa_offer_owners_session_idx ON openappa_offer_owners (organization_id, session_id, caller_id);

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
    CONSTRAINT openappa_operations_pk PRIMARY KEY (session_id, operation_id),
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
    CONSTRAINT openappa_results_pk PRIMARY KEY (session_id, tool_call_id),
    CONSTRAINT openappa_results_status CHECK (
        (status = 'pending' AND approved_output IS NULL AND decision IS NULL)
        OR (status = 'complete' AND approved_output IS NOT NULL AND decision IS NOT NULL)
    )
);
CREATE INDEX openappa_results_pending_idx ON openappa_processed_results (root) WHERE status = 'pending';
