CREATE TABLE IF NOT EXISTS status_transitions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    monitor_id INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
    status     TEXT    NOT NULL,
    message    TEXT    NOT NULL,
    at         INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX IF NOT EXISTS idx_transitions_monitor_at ON status_transitions(monitor_id, at);

CREATE TABLE IF NOT EXISTS check_samples (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    monitor_id      INTEGER NOT NULL REFERENCES monitors(id) ON DELETE CASCADE,
    status          TEXT    NOT NULL,
    message         TEXT    NOT NULL,
    components_json TEXT    NOT NULL,
    at              INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX IF NOT EXISTS idx_samples_monitor_at ON check_samples(monitor_id, at);
