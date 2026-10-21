-- 0001 initial schema (sqlite), forward-only
CREATE TABLE faxes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  call_id TEXT NOT NULL,
  trunk TEXT NOT NULL,
  ani TEXT,
  ani_source TEXT,
  dnis TEXT,
  dnis_source TEXT,
  remote_ident TEXT,
  started_at TEXT NOT NULL,
  answered_at TEXT,
  ended_at TEXT,
  duration_sec INTEGER,
  hangup_direction TEXT,
  hangup_code TEXT,
  hangup_text TEXT,
  sip_status INTEGER,
  t30_code INTEGER,
  fax_status TEXT NOT NULL,
  pages INTEGER NOT NULL DEFAULT 0,
  resolution TEXT,
  bit_rate INTEGER,
  compression TEXT,
  ecm INTEGER NOT NULL DEFAULT 0,
  tiff_path TEXT,
  tiff_size INTEGER,
  pdf_path TEXT,
  pdf_size INTEGER,
  files_purged_at TEXT,
  is_read INTEGER NOT NULL DEFAULT 0,
  read_changed_by TEXT,
  read_changed_at TEXT,
  note TEXT,
  note_updated_by TEXT,
  note_updated_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CONSTRAINT uq_faxes_call_id UNIQUE (call_id)
);

CREATE TABLE settings (
  name TEXT NOT NULL PRIMARY KEY,
  value TEXT NOT NULL,
  updated_by TEXT,
  updated_at TEXT NOT NULL
);

CREATE TABLE delivery_attempts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  fax_id INTEGER NOT NULL,
  target TEXT NOT NULL,
  kind TEXT NOT NULL,
  attempt INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL,
  last_error TEXT,
  next_try_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
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
