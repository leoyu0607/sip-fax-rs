-- 0003 the modem and the transport of a fax (postgres), forward-only (M5e)
ALTER TABLE faxes ADD COLUMN modem VARCHAR(16);
ALTER TABLE faxes ADD COLUMN media VARCHAR(8);
