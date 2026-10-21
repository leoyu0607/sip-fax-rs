-- 0001 initial schema (mysql), forward-only
CREATE TABLE faxes (
  id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  call_id VARCHAR(255) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL,
  trunk VARCHAR(64) NOT NULL,
  ani VARCHAR(64),
  ani_source VARCHAR(16),
  dnis VARCHAR(64),
  dnis_source VARCHAR(16),
  remote_ident VARCHAR(32),
  started_at DATETIME(3) NOT NULL,
  answered_at DATETIME(3),
  ended_at DATETIME(3),
  duration_sec INT,
  hangup_direction VARCHAR(8),
  hangup_code VARCHAR(32),
  hangup_text VARCHAR(255),
  sip_status INT,
  t30_code INT,
  fax_status VARCHAR(12) NOT NULL,
  pages INT NOT NULL DEFAULT 0,
  resolution VARCHAR(12),
  bit_rate INT,
  compression VARCHAR(8),
  ecm BOOLEAN NOT NULL DEFAULT 0,
  tiff_path VARCHAR(1024),
  tiff_size BIGINT,
  pdf_path VARCHAR(1024),
  pdf_size BIGINT,
  files_purged_at DATETIME(3),
  is_read BOOLEAN NOT NULL DEFAULT 0,
  read_changed_by VARCHAR(64),
  read_changed_at DATETIME(3),
  note TEXT,
  note_updated_by VARCHAR(64),
  note_updated_at DATETIME(3),
  created_at DATETIME(3) NOT NULL,
  updated_at DATETIME(3) NOT NULL,
  CONSTRAINT uq_faxes_call_id UNIQUE (call_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE settings (
  name VARCHAR(64) NOT NULL PRIMARY KEY,
  value TEXT NOT NULL,
  updated_by VARCHAR(64),
  updated_at DATETIME(3) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE delivery_attempts (
  id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  fax_id BIGINT NOT NULL,
  target VARCHAR(255) NOT NULL,
  kind VARCHAR(16) NOT NULL,
  attempt INT NOT NULL DEFAULT 0,
  status VARCHAR(12) NOT NULL,
  last_error TEXT,
  next_try_at DATETIME(3),
  created_at DATETIME(3) NOT NULL,
  updated_at DATETIME(3) NOT NULL,
  CONSTRAINT fk_delivery_fax FOREIGN KEY (fax_id) REFERENCES faxes (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE INDEX idx_faxes_started ON faxes (started_at DESC, id DESC);
CREATE INDEX idx_faxes_read_started ON faxes (is_read, started_at DESC);
CREATE INDEX idx_faxes_status ON faxes (fax_status);
CREATE INDEX idx_faxes_ani ON faxes (ani);
CREATE INDEX idx_faxes_dnis ON faxes (dnis);
CREATE INDEX idx_faxes_files_purged ON faxes (files_purged_at);
CREATE INDEX idx_faxes_ended ON faxes (ended_at);
CREATE INDEX idx_delivery_fax ON delivery_attempts (fax_id);
CREATE INDEX idx_delivery_due ON delivery_attempts (status, next_try_at);
