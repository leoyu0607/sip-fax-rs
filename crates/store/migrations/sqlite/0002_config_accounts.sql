-- 0002 runtime configuration in the database, accounts and sessions (sqlite), forward-only (M4a)
CREATE TABLE config_sections (
  name TEXT NOT NULL PRIMARY KEY,
  body TEXT NOT NULL,
  version INTEGER NOT NULL,
  updated_by TEXT,
  updated_at TEXT NOT NULL
);

CREATE TABLE config_audit (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  section TEXT NOT NULL,
  action TEXT NOT NULL,
  before_json TEXT,
  after_json TEXT,
  changed_by TEXT NOT NULL,
  at TEXT NOT NULL
);

CREATE TABLE users (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  username TEXT NOT NULL,
  display_name TEXT,
  role TEXT NOT NULL,
  password_hash TEXT NOT NULL,
  locale TEXT,
  disabled INTEGER NOT NULL DEFAULT 0,
  must_change_password INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_login_at TEXT,
  CONSTRAINT uq_users_username UNIQUE (username)
);

CREATE TABLE sessions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  token_hash TEXT NOT NULL,
  user_id INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  last_seen_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  ip TEXT,
  user_agent TEXT,
  CONSTRAINT uq_sessions_token UNIQUE (token_hash),
  CONSTRAINT fk_sessions_user FOREIGN KEY (user_id) REFERENCES users (id)
);

CREATE INDEX idx_config_audit_at ON config_audit (at DESC, id DESC);
CREATE INDEX idx_sessions_user ON sessions (user_id);
CREATE INDEX idx_sessions_expires ON sessions (expires_at);

-- the retention settings of M3 move into the `retention` section (missing keys take the defaults)
INSERT INTO config_sections (name, body, version, updated_by, updated_at)
SELECT 'retention',
  '{"files_days":' || COALESCE((SELECT value FROM settings WHERE name = 'retention.files_days'), '90')
  || ',"records_days":' || COALESCE((SELECT value FROM settings WHERE name = 'retention.records_days'), '365')
  || ',"run_at":"' || COALESCE((SELECT value FROM settings WHERE name = 'retention.run_at'), '03:17') || '"}',
  1, 'migration', strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE EXISTS (SELECT 1 FROM settings WHERE name LIKE 'retention.%')
  AND NOT EXISTS (SELECT 1 FROM config_sections WHERE name = 'retention');

DELETE FROM settings WHERE name LIKE 'retention.%';
