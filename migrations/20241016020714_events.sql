-- Add migration script here

CREATE TABLE IF NOT EXISTS events (
    id TEXT NOT NULL,
    ts bigint NOT NULL,
    kind int NOT NULL,
    src TEXT NOT NULL,
    dest TEXT NOT NULL,
    content TEXT,
    comfirmed BOOL,
    UNIQUE (id)
);

