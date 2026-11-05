-- Derived from binwiederhier/ntfy v2.37.2 (Apache-2.0); see NOTICE.
-- Alter user table: Add provisioned column
		ALTER TABLE user RENAME TO user_old;
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
		INSERT INTO user
		SELECT
		    id,
		    tier_id,
		    user,
		    pass,
		    role,
		    prefs,
		    sync_topic,
		    0, -- provisioned
		    stats_messages,
		    stats_emails,
		    stats_calls,
		    stripe_customer_id,
		    stripe_subscription_id,
		    stripe_subscription_status,
		    stripe_subscription_interval,
		    stripe_subscription_paid_until,
		    stripe_subscription_cancel_at,
		    created,
		    deleted
		FROM user_old;
		DROP TABLE user_old;

		-- Alter user_access table: Add provisioned column
		ALTER TABLE user_access RENAME TO user_access_old;
		CREATE TABLE user_access (
			user_id TEXT NOT NULL,
			topic TEXT NOT NULL,
			read INT NOT NULL,
			write INT NOT NULL,
			owner_user_id INT,
			provisioned INTEGER NOT NULL,
			PRIMARY KEY (user_id, topic),
			FOREIGN KEY (user_id) REFERENCES user (id) ON DELETE CASCADE,
			FOREIGN KEY (owner_user_id) REFERENCES user (id) ON DELETE CASCADE
		);
		INSERT INTO user_access SELECT *, 0 FROM user_access_old;
		DROP TABLE user_access_old;

		-- Alter user_token table: Add provisioned column
		ALTER TABLE user_token RENAME TO user_token_old;
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
		INSERT INTO user_token SELECT *, 0 FROM user_token_old;
		DROP TABLE user_token_old;

		-- Recreate indices
		CREATE UNIQUE INDEX idx_user ON user (user);
		CREATE UNIQUE INDEX idx_user_stripe_customer_id ON user (stripe_customer_id);
		CREATE UNIQUE INDEX idx_user_stripe_subscription_id ON user (stripe_subscription_id);
		CREATE UNIQUE INDEX idx_user_token ON user_token (token);
