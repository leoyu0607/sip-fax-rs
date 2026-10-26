-- 0003 the modem and the transport of a fax (mssql), forward-only (M5e)
ALTER TABLE faxes ADD modem NVARCHAR(16) NULL, media NVARCHAR(8) NULL;
