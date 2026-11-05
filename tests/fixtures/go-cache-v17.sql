-- From ntfy v2.37.2, Apache-2.0; see NOTICE.

		CREATE TABLE IF NOT EXISTS messages (
			id INTEGER PRIMARY KEY AUTOINCREMENT,
			mid TEXT NOT NULL,
			sequence_id TEXT NOT NULL,
			time INT NOT NULL,
			event TEXT NOT NULL,
			expires INT NOT NULL,
			topic TEXT NOT NULL,
			message TEXT NOT NULL,
			title TEXT NOT NULL,
			priority INT NOT NULL,
			tags TEXT NOT NULL,
			click TEXT NOT NULL,
			icon TEXT NOT NULL,
			actions TEXT NOT NULL,
			attachment_name TEXT NOT NULL,
			attachment_type TEXT NOT NULL,
			attachment_size INT NOT NULL,
			attachment_expires INT NOT NULL,
			attachment_url TEXT NOT NULL,
			attachment_deleted INT NOT NULL,
			sender TEXT NOT NULL,
			user TEXT NOT NULL,
			content_type TEXT NOT NULL,
			encoding TEXT NOT NULL,
			published INT NOT NULL
		);
		CREATE INDEX IF NOT EXISTS idx_mid ON messages (mid);
		CREATE INDEX IF NOT EXISTS idx_sequence_id ON messages (sequence_id);
		CREATE INDEX IF NOT EXISTS idx_time ON messages (time);
		CREATE INDEX IF NOT EXISTS idx_topic ON messages (topic);
		CREATE INDEX IF NOT EXISTS idx_topic_published_time ON messages (topic, published, time);
		CREATE INDEX IF NOT EXISTS idx_expires ON messages (expires);
		CREATE INDEX IF NOT EXISTS idx_sender ON messages (sender);
		CREATE INDEX IF NOT EXISTS idx_user ON messages (user);
		CREATE INDEX IF NOT EXISTS idx_attachment_expires ON messages (attachment_expires);
		CREATE TABLE IF NOT EXISTS stats (
			key TEXT PRIMARY KEY,
			value INT
		);
		INSERT INTO stats (key, value) VALUES ('messages', 0);
	
CREATE TABLE schemaVersion(id INT PRIMARY KEY, version INT NOT NULL, min_version INT NOT NULL DEFAULT 1);
INSERT INTO schemaVersion VALUES(1,17,1);
