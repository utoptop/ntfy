-- Derived from binwiederhier/ntfy v2.37.2 (Apache-2.0); see NOTICE.
CREATE TABLE IF NOT EXISTS topic (
			topic TEXT NOT NULL PRIMARY KEY,
			rate_visitor_user_id TEXT NOT NULL,
			rate_visitor_ip TEXT NOT NULL,
			first_seen_at INT NOT NULL,
			subscribed_at INT NOT NULL,
			updated_at INT NOT NULL
		);
		CREATE INDEX IF NOT EXISTS idx_topic_updated_at ON topic (updated_at);
