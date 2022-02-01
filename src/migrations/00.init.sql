CREATE TABLE pod (
     id VARCHAR(255) NOT NULL,
     created_at datetime NOT NULL,
     updated_at datetime DEFAULT NULL,
     status VARCHAR(255) NOT NULL,
     namespace varchar(255) NOT NULL,
     runtime varchar(255) NOT NULL,
     name varchar(255),
     image text,
     replicas int,
     ports text,
     labels JSON
);