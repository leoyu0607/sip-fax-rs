-- 0004 the integration interface (mysql / mariadb), forward-only (M6a, ADR-070): API keys, the
-- view fax_records (the public, stable shape of a fax: columns are only ever added) and the index
-- of the change feed. Every statement is safe to run again: CREATE TABLE IF NOT EXISTS, CREATE OR
-- REPLACE VIEW, and an ADD INDEX that finds the index there already ("Duplicate key name") counts
-- as done (migrate.rs).
CREATE TABLE IF NOT EXISTS api_keys (
  id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  name VARCHAR(48) NOT NULL,
  token_hash CHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
  prefix VARCHAR(16) NOT NULL,
  scopes VARCHAR(64) NOT NULL,
  created_by VARCHAR(64) NOT NULL,
  created_at DATETIME(3) NOT NULL,
  expires_at DATETIME(3),
  revoked_at DATETIME(3),
  revoked_by VARCHAR(64),
  last_used_at DATETIME(3),
  last_used_ip VARCHAR(64),
  UNIQUE KEY uq_api_keys_name (name),
  UNIQUE KEY uq_api_keys_token (token_hash)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

ALTER TABLE faxes ADD INDEX idx_faxes_updated (updated_at, id);

CREATE OR REPLACE VIEW fax_records AS
SELECT id, call_id, trunk, ani, ani_source, dnis, dnis_source, remote_ident,
  started_at, answered_at, ended_at, duration_sec,
  hangup_direction, hangup_code, hangup_text, sip_status, t30_code,
  fax_status, pages, resolution, bit_rate, compression, ecm, modem, media,
  CASE WHEN tiff_path IS NOT NULL AND files_purged_at IS NULL THEN TRUE ELSE FALSE END AS has_tiff,
  CASE WHEN pdf_path IS NOT NULL AND files_purged_at IS NULL THEN TRUE ELSE FALSE END AS has_pdf,
  tiff_size, pdf_size, files_purged_at,
  is_read, read_changed_by, read_changed_at, note, note_updated_by, note_updated_at,
  created_at, updated_at
FROM faxes;
