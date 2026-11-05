-- Derived from binwiederhier/ntfy v2.37.2 (Apache-2.0); see NOTICE.
ALTER TABLE user ADD COLUMN stripe_subscription_interval TEXT;
		ALTER TABLE tier RENAME COLUMN stripe_price_id TO stripe_monthly_price_id;
		ALTER TABLE tier ADD COLUMN stripe_yearly_price_id TEXT;
		DROP INDEX IF EXISTS idx_tier_price_id;
		CREATE UNIQUE INDEX idx_tier_stripe_monthly_price_id ON tier (stripe_monthly_price_id);
		CREATE UNIQUE INDEX idx_tier_stripe_yearly_price_id ON tier (stripe_yearly_price_id);
