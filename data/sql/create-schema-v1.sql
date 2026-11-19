CREATE TABLE snapshot (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    data TEXT NOT NULL
);

CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    occurred_at INTEGER NOT NULL,
    kind TEXT NOT NULL,
    data TEXT NOT NULL
);

PRAGMA user_version = 1;
