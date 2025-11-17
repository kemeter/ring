-- Add health_checks column to deployment table
ALTER TABLE deployment ADD COLUMN health_checks JSON DEFAULT '[]';

-- Create health_check table to store health check execution results
CREATE TABLE health_check (
    id VARCHAR(255) PRIMARY KEY NOT NULL,
    deployment_id VARCHAR(255) NOT NULL,
    check_type VARCHAR(50) NOT NULL,
    status VARCHAR(20) NOT NULL,
    message TEXT DEFAULT NULL,
    created_at datetime NOT NULL,
    started_at datetime NOT NULL,
    finished_at datetime NOT NULL
);

-- Create index for better query performance
CREATE INDEX idx_health_check_deployment_id ON health_check(deployment_id);
CREATE INDEX idx_health_check_started_at ON health_check(started_at);