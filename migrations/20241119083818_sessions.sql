-- Add migration script here
-- nostr pubkey
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT NOT NULL,
    ts bigint NOT NULL,
    name TEXT NOT NULL,
    pubkey TEXT NOT NULL,
    onetimekey TEXT NOT NULL,
    UNIQUE (id)
);

CREATE TABLE IF NOT EXISTS receivers (
    id TEXT NOT NULL,
    ts bigint NOT NULL,
    pubkey TEXT NOT NULL,
    address TEXT NOT NULL,
    UNIQUE (id, address)
);
