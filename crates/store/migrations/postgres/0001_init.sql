-- 0001 initial schema (postgres), forward-only
CREATE TABLE faxes (
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  call_id VARCHAR(255) NOT NULL,
  trunk VARCHAR(64) NOT NULL,
  ani VARCHAR(64),
  ani_source VARCHAR(16),
  dnis VARCHAR(64),
  dnis_source VARCHAR(16),
  remote_ident VARCHAR(32),
  started_at TIMESTAMPTZ NOT NULL,
  answered_at TIMESTAMPTZ,
  ended_at TIMESTAMPTZ,
  duration_sec INTEGER,
  hangup_direction VARCHAR(8),
  hangup_code VARCHAR(32),
  hangup_text VARCHAR(255),
  sip_status INTEGER,
  t30_code INTEGER,
  fax_status VARCHAR(12) NOT NULL,
  pages INTEGER NOT NULL DEFAULT 0,
  resolution VARCHAR(12),
  bit_rate INTEGER,
  compression VARCHAR(8),
  ecm BOOLEAN NOT NULL DEFAULT FALSE,
  tiff_path VARCHAR(1024),
  tiff_size BIGINT,
  pdf_path VARCHAR(1024),
  pdf_size BIGINT,
  files_purged_at TIMESTAMPTZ,
  is_read BOOLEAN NOT NULL DEFAULT FALSE,
  read_changed_by VARCHAR(64),
  read_changed_at TIMESTAMPTZ,
  note TEXT,
  note_updated_by VARCHAR(64),
  note_updated_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL,
  CONSTRAINT uq_faxes_call_id UNIQUE (call_id)
);

CREATE TABLE settings (
  name VARCHAR(64) NOT NULL PRIMARY KEY,
  value TEXT NOT NULL,
  updated_by VARCHAR(64),
  updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE delivery_attempts (
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  fax_id BIGINT NOT NULL,
  target VARCHAR(255) NOT NULL,
  kind VARCHAR(16) NOT NULL,
  attempt INTEGER NOT NULL DEFAULT 0,
  status VARCHAR(12) NOT NULL,
  last_error TEXT,
  next_try_at TIMESTAMPTZ,
  created_at TIMESTAMPTZ NOT NULL,
  updated_at TIMESTAMPTZ NOT NULL,
  CONSTRAINT fk_delivery_fax FOREIGN KEY (fax_id) REFERENCES faxes (id)
);

CREATE INDEX idx_faxes_started ON faxes (started_at DESC, id DESC);
CREATE INDEX idx_faxes_read_started ON faxes (is_read, started_at DESC);
CREATE INDEX idx_faxes_status ON faxes (fax_status);
CREATE INDEX idx_faxes_ani ON faxes (ani);
CREATE INDEX idx_faxes_dnis ON faxes (dnis);
CREATE INDEX idx_faxes_files_purged ON faxes (files_purged_at);
CREATE INDEX idx_faxes_ended ON faxes (ended_at);
CREATE INDEX idx_delivery_fax ON delivery_attempts (fax_id);
CREATE INDEX idx_delivery_due ON delivery_attempts (status, next_try_at);
