CREATE TABLE deployment (
     id VARCHAR(255) NOT NULL,
     created_at datetime NOT NULL,
     updated_at datetime DEFAULT NULL,
     status VARCHAR(255) NOT NULL,
     namespace varchar(255) NOT NULL,
     runtime varchar(255) NOT NULL,
     kind varchar(255) NOT NULL,
     name varchar(255),
     image text,
     replicas int,
     ports text,
     labels JSON,
     secrets JSON,
     volumes JSON
);

CREATE TABLE user (
    id VARCHAR(255) NOT NULL,
    created_at datetime NOT NULL,
    updated_at datetime DEFAULT NULL,
    status VARCHAR(255) NOT NULL,
    username VARCHAR(255) NOT NULL,
    password VARCHAR(255) NOT NULL,
    login_at datetime DEFAULT NULL,
    token text DEFAULT NULL
);

INSERT INTO user (
    id,
    created_at,
    updated_at,
    status,
    username,
    password,
    token,
    login_at
)
VALUES (
    '1c5a5fe9-84e0-4a18-821e-8058232c2c23',
    '2022-07-12',
    '2022-07-12',
    'active',
    'admin',
    '$argon2i$v=19$m=4096,t=3,p=1$cmFuZG9tc2FsdA$JYSqhpZWaZIlroh1VY0p+Hp0q9VX6T9hV4gauhJNOt4',
    'dba77b04-72aa-4fb5-9bf7-2bd8ac8ead92',
    '2022-07-12'
);
