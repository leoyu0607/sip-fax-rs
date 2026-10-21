-- 0002 runtime configuration in the database, accounts and sessions (mysql / mariadb), forward-only (M4a).
-- Every statement is safe to run again (CREATE TABLE IF NOT EXISTS with the indexes inside, the data
-- move is guarded by NOT EXISTS and the DELETE is idempotent).
CREATE TABLE IF NOT EXISTS config_sections (
  name VARCHAR(64) NOT NULL PRIMARY KEY,
  body MEDIUMTEXT NOT NULL,
  version INT NOT NULL,
  updated_by VARCHAR(64),
  updated_at DATETIME(3) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS config_audit (
  id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  section VARCHAR(64) NOT NULL,
  action VARCHAR(16) NOT NULL,
  before_json MEDIUMTEXT,
  after_json MEDIUMTEXT,
  changed_by VARCHAR(64) NOT NULL,
  at DATETIME(3) NOT NULL,
  INDEX idx_config_audit_at (at, id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS users (
  id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  username VARCHAR(64) NOT NULL,
  display_name VARCHAR(128),
  role VARCHAR(8) NOT NULL,
  password_hash VARCHAR(255) NOT NULL,
  locale VARCHAR(8),
  disabled BOOLEAN NOT NULL DEFAULT 0,
  must_change_password BOOLEAN NOT NULL DEFAULT 0,
  created_at DATETIME(3) NOT NULL,
  updated_at DATETIME(3) NOT NULL,
  last_login_at DATETIME(3),
  CONSTRAINT uq_users_username UNIQUE (username)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS sessions (
  id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  token_hash CHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
  user_id BIGINT NOT NULL,
  created_at DATETIME(3) NOT NULL,
  last_seen_at DATETIME(3) NOT NULL,
  expires_at DATETIME(3) NOT NULL,
  ip VARCHAR(64),
  user_agent VARCHAR(255),
  CONSTRAINT uq_sessions_token UNIQUE (token_hash),
  CONSTRAINT fk_sessions_user FOREIGN KEY (user_id) REFERENCES users (id),
  INDEX idx_sessions_user (user_id),
  INDEX idx_sessions_expires (expires_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- the retention settings of M3 move into the `retention` section (missing keys take the defaults)
INSERT INTO config_sections (name, body, version, updated_by, updated_at)
SELECT 'retention',
  CONCAT('{"files_days":', COALESCE((SELECT value FROM settings WHERE name = 'retention.files_days'), '90'),
  ',"records_days":', COALESCE((SELECT value FROM settings WHERE name = 'retention.records_days'), '365'),
  ',"run_at":"', COALESCE((SELECT value FROM settings WHERE name = 'retention.run_at'), '03:17'), '"}'),
  1, 'migration', UTC_TIMESTAMP(3)
FROM DUAL
WHERE EXISTS (SELECT 1 FROM settings WHERE name LIKE 'retention.%')
  AND NOT EXISTS (SELECT 1 FROM config_sections WHERE name = 'retention');

DELETE FROM settings WHERE name LIKE 'retention.%';
