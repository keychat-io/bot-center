-- Add migration script here

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT NOT NULL, --nostr pubkey
    local TEXT NOT NULL, --nostr pubkey local
    ts bigint NOT NULL,
    pubkey TEXT NOT NULL, --signalid
    name TEXT NOT NULL,
    onetimekey TEXT NOT NULL,
    UNIQUE (id, local)
);

CREATE TABLE IF NOT EXISTS receivers (
    id TEXT NOT NULL,
    local TEXT NOT NULL,
    ts bigint NOT NULL,
    pubkey TEXT NOT NULL,
    address TEXT NOT NULL,
    UNIQUE (id, local, address)
);
