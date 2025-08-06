-- Create deployment_event table for tracking deployment lifecycle events
CREATE TABLE deployment_event (
    id VARCHAR(255) PRIMARY KEY,
    deployment_id VARCHAR(255) NOT NULL,
    timestamp datetime NOT NULL,
    level VARCHAR(20) NOT NULL,
    message TEXT NOT NULL,
    component VARCHAR(50) NOT NULL,
    reason VARCHAR(100)
);

-- Create indexes for performance
CREATE INDEX idx_deployment_events_deployment_id ON deployment_event(deployment_id);
CREATE INDEX idx_deployment_events_timestamp ON deployment_event(timestamp);
CREATE INDEX idx_deployment_events_level ON deployment_event(level);

-- Add last_event_at column to deployment table for quick access
ALTER TABLE deployment ADD COLUMN last_event_at datetime DEFAULT NULL;