-- Derived from binwiederhier/ntfy v2.37.2 (Apache-2.0); see NOTICE.
CREATE TABLE IF NOT EXISTS user_email (
			user_id TEXT NOT NULL,
			email TEXT NOT NULL,
			PRIMARY KEY (user_id, email),
			FOREIGN KEY (user_id) REFERENCES user (id) ON DELETE CASCADE
		);
