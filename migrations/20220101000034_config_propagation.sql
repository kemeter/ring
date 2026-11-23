-- A config update whose `on_change` has not reached the deployments mounting
-- the config yet. Written with the update, removed once the propagation ran:
-- one left behind (the request was cancelled, the server stopped) is resumed
-- by the scheduler. `version` is the config's `updated_at` it was written for.
CREATE TABLE IF NOT EXISTS config_propagation (
    config_id VARCHAR(255) PRIMARY KEY NOT NULL,
    version TEXT NOT NULL,
    skip_deployments JSON NOT NULL DEFAULT '[]'
);
