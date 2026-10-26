-- 0003 the modem and the transport of a fax (sqlite), forward-only (M5e)
ALTER TABLE faxes ADD COLUMN modem TEXT;
ALTER TABLE faxes ADD COLUMN media TEXT;
