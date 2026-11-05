-- Derived from binwiederhier/ntfy v2.37.2 (Apache-2.0); see NOTICE.
UPDATE user_access SET topic = REPLACE(topic, '_', '\_');
