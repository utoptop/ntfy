-- Derived from binwiederhier/ntfy v2.37.2 (Apache-2.0); see NOTICE.
ALTER TABLE user_email ADD COLUMN is_primary INT NOT NULL DEFAULT (0);
		CREATE UNIQUE INDEX idx_user_email_primary_user ON user_email (user_id) WHERE is_primary = 1;
		CREATE UNIQUE INDEX idx_user_email_primary_addr ON user_email (email) WHERE is_primary = 1;
		CREATE TABLE IF NOT EXISTS user_magic_link (
			token_hash TEXT NOT NULL,
			kind TEXT NOT NULL,
			user_id TEXT NOT NULL,
			email TEXT,
			expires INT NOT NULL,
			created INT NOT NULL,
			PRIMARY KEY (token_hash),
			FOREIGN KEY (user_id) REFERENCES user (id) ON DELETE CASCADE
		);
		CREATE INDEX idx_magic_link_user_kind ON user_magic_link (user_id, kind);
