-- 0002 runtime configuration in the database, accounts and sessions (postgres), forward-only (M4a)
CREATE TABLE config_sections (
  name VARCHAR(64) NOT NULL PRIMARY KEY,
  body TEXT NOT NULL,
  version INTEGER NOT NULL,
  updated_by VARCHAR(64),
  updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE config_audit (
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  section VARCHAR(64) NOT NULL,
  action VARCHAR(16) NOT NULL,
  before_json TEXT,
  after_json TEXT,
  changed_by VARCHAR(64) NOT NULL,
  at TIMESTAMPTZ NOT NULL
);

CREATE TABLE users (
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  username VARCHAR(64) NOT NULL,
  display_name VARCHAR(128),
  role VARCHAR(8) NOT NULL,
  password_hash VARCHAR(255) NOT NULL,
  locale VARCHAR(8),
  disabled BOOLEAN NOT NULL DEFAULT FALSE,
  must_change_password BOOLEAN NOT NULL DEFAULT FALSE,
  created_at TIMESTAMPTZ NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL,
  last_login_at TIMESTAMPTZ,
  CONSTRAINT uq_users_username UNIQUE (username)
);

CREATE TABLE sessions (
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  token_hash CHAR(64) NOT NULL,
  user_id BIGINT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL,
  last_seen_at TIMESTAMPTZ NOT NULL,
  expires_at TIMESTAMPTZ NOT NULL,
  ip VARCHAR(64),
  user_agent VARCHAR(255),
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
  1, 'migration', now()
WHERE EXISTS (SELECT 1 FROM settings WHERE name LIKE 'retention.%')
  AND NOT EXISTS (SELECT 1 FROM config_sections WHERE name = 'retention');

DELETE FROM settings WHERE name LIKE 'retention.%';
