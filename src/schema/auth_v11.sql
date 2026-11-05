-- Derived from binwiederhier/ntfy v2.37.2 (Apache-2.0); see NOTICE.
CREATE TABLE IF NOT EXISTS tier (
			id TEXT PRIMARY KEY,
			code TEXT NOT NULL,
			name TEXT NOT NULL,
			messages_limit INT NOT NULL,
			messages_expiry_duration INT NOT NULL,
			emails_limit INT NOT NULL,
			calls_limit INT NOT NULL,
			reservations_limit INT NOT NULL,
			attachment_file_size_limit INT NOT NULL,
			attachment_total_size_limit INT NOT NULL,
			attachment_expiry_duration INT NOT NULL,
			attachment_bandwidth_limit INT NOT NULL,
			stripe_monthly_price_id TEXT,
			stripe_yearly_price_id TEXT
		);
		CREATE UNIQUE INDEX idx_tier_code ON tier (code);
		CREATE UNIQUE INDEX idx_tier_stripe_monthly_price_id ON tier (stripe_monthly_price_id);
		CREATE UNIQUE INDEX idx_tier_stripe_yearly_price_id ON tier (stripe_yearly_price_id);
		CREATE TABLE IF NOT EXISTS user (
		    id TEXT PRIMARY KEY,
			tier_id TEXT,
			user TEXT NOT NULL,
			pass TEXT NOT NULL,
			role TEXT CHECK (role IN ('anonymous', 'admin', 'user')) NOT NULL,
			prefs JSON NOT NULL DEFAULT '{}',
			sync_topic TEXT NOT NULL,
			provisioned INT NOT NULL,
			stats_messages INT NOT NULL DEFAULT (0),
			stats_emails INT NOT NULL DEFAULT (0),
			stats_calls INT NOT NULL DEFAULT (0),
			stripe_customer_id TEXT,
			stripe_subscription_id TEXT,
			stripe_subscription_status TEXT,
			stripe_subscription_interval TEXT,
			stripe_subscription_paid_until INT,
			stripe_subscription_cancel_at INT,
			created INT NOT NULL,
			deleted INT,
		    FOREIGN KEY (tier_id) REFERENCES tier (id)
		);
		CREATE UNIQUE INDEX idx_user ON user (user);
		CREATE UNIQUE INDEX idx_user_stripe_customer_id ON user (stripe_customer_id);
		CREATE UNIQUE INDEX idx_user_stripe_subscription_id ON user (stripe_subscription_id);
		CREATE TABLE IF NOT EXISTS user_access (
			user_id TEXT NOT NULL,
			topic TEXT NOT NULL,
			read INT NOT NULL,
			write INT NOT NULL,
			owner_user_id INT,
			provisioned INT NOT NULL,
			PRIMARY KEY (user_id, topic),
			FOREIGN KEY (user_id) REFERENCES user (id) ON DELETE CASCADE,
		    FOREIGN KEY (owner_user_id) REFERENCES user (id) ON DELETE CASCADE
		);
		CREATE TABLE IF NOT EXISTS user_token (
			user_id TEXT NOT NULL,
			token TEXT NOT NULL,
			label TEXT NOT NULL,
			last_access INT NOT NULL,
			last_origin TEXT NOT NULL,
			expires INT NOT NULL,
			provisioned INT NOT NULL,
			PRIMARY KEY (user_id, token),
			FOREIGN KEY (user_id) REFERENCES user (id) ON DELETE CASCADE
		);
		CREATE UNIQUE INDEX idx_user_token ON user_token (token);
		CREATE TABLE IF NOT EXISTS user_phone (
			user_id TEXT NOT NULL,
			phone_number TEXT NOT NULL,
			PRIMARY KEY (user_id, phone_number),
			FOREIGN KEY (user_id) REFERENCES user (id) ON DELETE CASCADE
		);
		CREATE TABLE IF NOT EXISTS user_email (
			user_id TEXT NOT NULL,
			email TEXT NOT NULL,
			is_primary INT NOT NULL DEFAULT (0),
			PRIMARY KEY (user_id, email),
			FOREIGN KEY (user_id) REFERENCES user (id) ON DELETE CASCADE
		);
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
		INSERT INTO user (id, user, pass, role, sync_topic, provisioned, created)
		VALUES ('u_everyone', '*', '', 'anonymous', '', false, UNIXEPOCH())
		ON CONFLICT (id) DO NOTHING;
CREATE TABLE IF NOT EXISTS usage (
			key TEXT NOT NULL PRIMARY KEY,
			period_start INT NOT NULL,
			messages INT NOT NULL DEFAULT 0,
			emails INT NOT NULL DEFAULT 0,
			calls INT NOT NULL DEFAULT 0,
			updated_at INT NOT NULL
		);
		CREATE INDEX IF NOT EXISTS idx_usage_updated_at ON usage (updated_at);
CREATE TABLE IF NOT EXISTS topic (
			topic TEXT NOT NULL PRIMARY KEY,
			rate_visitor_user_id TEXT NOT NULL,
			rate_visitor_ip TEXT NOT NULL,
			first_seen_at INT NOT NULL,
			subscribed_at INT NOT NULL,
			updated_at INT NOT NULL
		);
		CREATE INDEX IF NOT EXISTS idx_topic_updated_at ON topic (updated_at);
