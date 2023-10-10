ALTER TABLE deployment ADD COLUMN restart_count INT DEFAULT 0;
ALTER TABLE deployment ADD COLUMN logs JSON;