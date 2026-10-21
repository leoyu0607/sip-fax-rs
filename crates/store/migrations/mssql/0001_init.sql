-- 0001 initial schema (mssql), forward-only
CREATE TABLE faxes (
  id BIGINT IDENTITY(1,1) PRIMARY KEY,
  call_id NVARCHAR(255) COLLATE Latin1_General_100_BIN2 NOT NULL,
  trunk NVARCHAR(64) NOT NULL,
  ani NVARCHAR(64),
  ani_source NVARCHAR(16),
  dnis NVARCHAR(64),
  dnis_source NVARCHAR(16),
  remote_ident NVARCHAR(32),
  started_at DATETIME2(3) NOT NULL,
  answered_at DATETIME2(3),
  ended_at DATETIME2(3),
  duration_sec INT,
  hangup_direction NVARCHAR(8),
  hangup_code NVARCHAR(32),
  hangup_text NVARCHAR(255),
  sip_status INT,
  t30_code INT,
  fax_status NVARCHAR(12) NOT NULL,
  pages INT NOT NULL DEFAULT 0,
  resolution NVARCHAR(12),
  bit_rate INT,
  compression NVARCHAR(8),
  ecm BIT NOT NULL DEFAULT 0,
  tiff_path NVARCHAR(1024),
  tiff_size BIGINT,
  pdf_path NVARCHAR(1024),
  pdf_size BIGINT,
  files_purged_at DATETIME2(3),
  is_read BIT NOT NULL DEFAULT 0,
  read_changed_by NVARCHAR(64),
  read_changed_at DATETIME2(3),
  note NVARCHAR(MAX),
  note_updated_by NVARCHAR(64),
  note_updated_at DATETIME2(3),
  created_at DATETIME2(3) NOT NULL,
  updated_at DATETIME2(3) NOT NULL,
  CONSTRAINT uq_faxes_call_id UNIQUE (call_id)
);

CREATE TABLE settings (
  name NVARCHAR(64) NOT NULL PRIMARY KEY,
  value NVARCHAR(MAX) NOT NULL,
  updated_by NVARCHAR(64),
  updated_at DATETIME2(3) NOT NULL
);

CREATE TABLE delivery_attempts (
  id BIGINT IDENTITY(1,1) PRIMARY KEY,
  fax_id BIGINT NOT NULL,
  target NVARCHAR(255) NOT NULL,
  kind NVARCHAR(16) NOT NULL,
  attempt INT NOT NULL DEFAULT 0,
  status NVARCHAR(12) NOT NULL,
  last_error NVARCHAR(MAX),
  next_try_at DATETIME2(3),
  created_at DATETIME2(3) NOT NULL,
  updated_at DATETIME2(3) NOT NULL,
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
