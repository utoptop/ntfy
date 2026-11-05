-- Derived from binwiederhier/ntfy v2.37.2 (Apache-2.0); see NOTICE.
ALTER TABLE tier ADD COLUMN calls_limit INT NOT NULL DEFAULT (0);
		ALTER TABLE user ADD COLUMN stats_calls INT NOT NULL DEFAULT (0);
		CREATE TABLE IF NOT EXISTS user_phone (
			user_id TEXT NOT NULL,
			phone_number TEXT NOT NULL,
			PRIMARY KEY (user_id, phone_number),
			FOREIGN KEY (user_id) REFERENCES user (id) ON DELETE CASCADE
		);
