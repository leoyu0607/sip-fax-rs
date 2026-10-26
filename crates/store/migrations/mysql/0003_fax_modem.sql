-- 0003 the modem and the transport of a fax (mysql / mariadb), forward-only (M5e).
-- One statement (atomic DDL). Run again after a failure between it and the version row, it
-- fails with "Duplicate column name", which the migration runner takes as done (migrate.rs).
ALTER TABLE faxes ADD COLUMN modem VARCHAR(16) NULL, ADD COLUMN media VARCHAR(8) NULL;
