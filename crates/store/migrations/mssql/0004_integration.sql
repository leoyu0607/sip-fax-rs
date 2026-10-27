-- 0004 the integration interface (mssql), forward-only (M6a, ADR-070): API keys, the view
-- fax_records (the public, stable shape of a fax: columns are only ever added) and the index of
-- the change feed.
CREATE TABLE api_keys (
  id BIGINT IDENTITY(1,1) PRIMARY KEY,
  name NVARCHAR(48) NOT NULL,
  token_hash CHAR(64) COLLATE Latin1_General_100_BIN2 NOT NULL,
  prefix NVARCHAR(16) NOT NULL,
  scopes NVARCHAR(64) NOT NULL,
  created_by NVARCHAR(64) NOT NULL,
  created_at DATETIME2(3) NOT NULL,
  expires_at DATETIME2(3),
  revoked_at DATETIME2(3),
  revoked_by NVARCHAR(64),
  last_used_at DATETIME2(3),
  last_used_ip NVARCHAR(64),
  CONSTRAINT uq_api_keys_name UNIQUE (name),
  CONSTRAINT uq_api_keys_token UNIQUE (token_hash)
);

CREATE INDEX idx_faxes_updated ON faxes (updated_at, id);

-- CREATE VIEW has to be the only statement of its batch: EXEC runs it as one.
EXEC(N'CREATE VIEW fax_records AS
SELECT id, call_id, trunk, ani, ani_source, dnis, dnis_source, remote_ident,
  started_at, answered_at, ended_at, duration_sec,
  hangup_direction, hangup_code, hangup_text, sip_status, t30_code,
  fax_status, pages, resolution, bit_rate, compression, ecm, modem, media,
  CAST(CASE WHEN tiff_path IS NOT NULL AND files_purged_at IS NULL THEN 1 ELSE 0 END AS BIT) AS has_tiff,
  CAST(CASE WHEN pdf_path IS NOT NULL AND files_purged_at IS NULL THEN 1 ELSE 0 END AS BIT) AS has_pdf,
  tiff_size, pdf_size, files_purged_at,
  is_read, read_changed_by, read_changed_at, note, note_updated_by, note_updated_at,
  created_at, updated_at
FROM faxes');
