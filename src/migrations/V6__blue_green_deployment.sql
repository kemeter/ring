-- Add Blue/Green deployment support with versioning
ALTER TABLE deployment ADD COLUMN predecessor_id VARCHAR(255) DEFAULT NULL;
ALTER TABLE deployment ADD COLUMN superseded_at DATETIME DEFAULT NULL;

-- Add foreign key constraint to link deployments (optional but good practice)
-- CREATE INDEX idx_deployment_predecessor ON deployment(predecessor_id);

-- Create an index on namespace + name + status for faster lookups
CREATE INDEX idx_deployment_namespace_name_status ON deployment(namespace, name, status);