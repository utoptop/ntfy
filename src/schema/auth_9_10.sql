-- Derived from binwiederhier/ntfy v2.37.2 (Apache-2.0); see NOTICE.
CREATE TABLE IF NOT EXISTS usage (
			key TEXT NOT NULL PRIMARY KEY,
			period_start INT NOT NULL,
			messages INT NOT NULL DEFAULT 0,
			emails INT NOT NULL DEFAULT 0,
			calls INT NOT NULL DEFAULT 0,
			updated_at INT NOT NULL
		);
		CREATE INDEX IF NOT EXISTS idx_usage_updated_at ON usage (updated_at);
UPDATE user SET stats_messages=0, stats_emails=0, stats_calls=0;
