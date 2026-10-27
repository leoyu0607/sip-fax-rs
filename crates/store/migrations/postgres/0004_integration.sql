-- 0004 the integration interface (postgres), forward-only (M6a, ADR-070): API keys, the view
-- fax_records (the public, stable shape of a fax: columns are only ever added) and the index of
-- the change feed.
CREATE TABLE api_keys (
  id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  name VARCHAR(48) NOT NULL,
  token_hash CHAR(64) NOT NULL,
  prefix VARCHAR(16) NOT NULL,
  scopes VARCHAR(64) NOT NULL,
  created_by VARCHAR(64) NOT NULL,
  created_at TIMESTAMPTZ NOT NULL,
  expires_at TIMESTAMPTZ,
  revoked_at TIMESTAMPTZ,
  revoked_by VARCHAR(64),
  last_used_at TIMESTAMPTZ,
  last_used_ip VARCHAR(64),
  CONSTRAINT uq_api_keys_name UNIQUE (name),
  CONSTRAINT uq_api_keys_token UNIQUE (token_hash)
);

CREATE INDEX idx_faxes_updated ON faxes (updated_at, id);

CREATE VIEW fax_records AS
SELECT id, call_id, trunk, ani, ani_source, dnis, dnis_source, remote_ident,
  started_at, answered_at, ended_at, duration_sec,
  hangup_direction, hangup_code, hangup_text, sip_status, t30_code,
  fax_status, pages, resolution, bit_rate, compression, ecm, modem, media,
  (tiff_path IS NOT NULL AND files_purged_at IS NULL) AS has_tiff,
  (pdf_path IS NOT NULL AND files_purged_at IS NULL) AS has_pdf,
  tiff_size, pdf_size, files_purged_at,
  is_read, read_changed_by, read_changed_at, note, note_updated_by, note_updated_at,
  created_at, updated_at
FROM faxes;
