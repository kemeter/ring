CREATE TABLE deployment (
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

CREATE TABLE user (
    id VARCHAR(255) NOT NULL,
    created_at datetime NOT NULL,
    updated_at datetime DEFAULT NULL,
    status VARCHAR(255) NOT NULL,
    username VARCHAR(255) NOT NULL,
    password VARCHAR(255) NOT NULL
);