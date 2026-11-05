-- Derived from binwiederhier/ntfy v2.37.2 (Apache-2.0); see NOTICE.
ALTER TABLE user_phone RENAME TO user_phone_old;
		CREATE TABLE user_phone (
			user_id TEXT NOT NULL,
			phone_number TEXT NOT NULL,
			PRIMARY KEY (user_id, phone_number),
			FOREIGN KEY (user_id) REFERENCES user (id) ON DELETE CASCADE
		);
		INSERT INTO user_phone (user_id, phone_number)
		SELECT user_id, phone_number FROM user_phone_old
		WHERE user_id IN (SELECT id FROM user); -- Drop orphaned rows that the broken foreign key failed to cascade-delete
		DROP TABLE user_phone_old;
