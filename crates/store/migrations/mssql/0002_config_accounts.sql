-- 0002 runtime configuration in the database, accounts and sessions (mssql), forward-only (M4a)
CREATE TABLE config_sections (
  name NVARCHAR(64) NOT NULL PRIMARY KEY,
  body NVARCHAR(MAX) NOT NULL,
  version INT NOT NULL,
  updated_by NVARCHAR(64),
  updated_at DATETIME2(3) NOT NULL
);

CREATE TABLE config_audit (
  id BIGINT IDENTITY(1,1) PRIMARY KEY,
  section NVARCHAR(64) NOT NULL,
  action NVARCHAR(16) NOT NULL,
  before_json NVARCHAR(MAX),
  after_json NVARCHAR(MAX),
  changed_by NVARCHAR(64) NOT NULL,
  at DATETIME2(3) NOT NULL
);

CREATE TABLE users (
  id BIGINT IDENTITY(1,1) PRIMARY KEY,
  username NVARCHAR(64) NOT NULL,
  display_name NVARCHAR(128),
  role NVARCHAR(8) NOT NULL,
  password_hash NVARCHAR(255) NOT NULL,
  locale NVARCHAR(8),
  disabled BIT NOT NULL DEFAULT 0,
  must_change_password BIT NOT NULL DEFAULT 0,
  created_at DATETIME2(3) NOT NULL,
  updated_at DATETIME2(3) NOT NULL,
  last_login_at DATETIME2(3),
  CONSTRAINT uq_users_username UNIQUE (username)
);

CREATE TABLE sessions (
  id BIGINT IDENTITY(1,1) PRIMARY KEY,
  token_hash CHAR(64) COLLATE Latin1_General_100_BIN2 NOT NULL,
  user_id BIGINT NOT NULL,
  created_at DATETIME2(3) NOT NULL,
  last_seen_at DATETIME2(3) NOT NULL,
  expires_at DATETIME2(3) NOT NULL,
  ip NVARCHAR(64),
  user_agent NVARCHAR(255),
  CONSTRAINT uq_sessions_token UNIQUE (token_hash),
  CONSTRAINT fk_sessions_user FOREIGN KEY (user_id) REFERENCES users (id)
);

CREATE INDEX idx_config_audit_at ON config_audit (at DESC, id DESC);
CREATE INDEX idx_sessions_user ON sessions (user_id);
CREATE INDEX idx_sessions_expires ON sessions (expires_at);

-- the retention settings of M3 move into the `retention` section (missing keys take the defaults)
INSERT INTO config_sections (name, body, version, updated_by, updated_at)
SELECT 'retention',
  CONCAT('{"files_days":', COALESCE((SELECT value FROM settings WHERE name = 'retention.files_days'), '90'),
  ',"records_days":', COALESCE((SELECT value FROM settings WHERE name = 'retention.records_days'), '365'),
  ',"run_at":"', COALESCE((SELECT value FROM settings WHERE name = 'retention.run_at'), '03:17'), '"}'),
  1, 'migration', SYSUTCDATETIME()
WHERE EXISTS (SELECT 1 FROM settings WHERE name LIKE 'retention.%')
  AND NOT EXISTS (SELECT 1 FROM config_sections WHERE name = 'retention');

DELETE FROM settings WHERE name LIKE 'retention.%';
