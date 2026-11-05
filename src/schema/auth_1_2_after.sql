-- Derived from binwiederhier/ntfy v2.37.2 (Apache-2.0); see NOTICE.
INSERT INTO user_access (user_id, topic, read, write)
		SELECT u.id, a.topic, a.read, a.write
		FROM user u
	 	JOIN access a ON u.user = a.user;

		DROP TABLE access;
		DROP TABLE user_old;
