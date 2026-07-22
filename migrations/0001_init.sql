CREATE TABLE monitors (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL,
    type_id       TEXT    NOT NULL,
    config_json   TEXT    NOT NULL,
    interval_secs INTEGER NOT NULL,
    enabled       INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE status_current (
    monitor_id      INTEGER PRIMARY KEY REFERENCES monitors(id) ON DELETE CASCADE,
    status          TEXT    NOT NULL,
    message         TEXT    NOT NULL,
    components_json TEXT    NOT NULL,
    updated_at      TEXT    NOT NULL DEFAULT (datetime('now'))
);
