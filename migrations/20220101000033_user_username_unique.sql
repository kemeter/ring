-- A username names one account.
--
-- Nothing enforced it, so `POST /users` with a taken username created a second
-- account: it could never log in (login finds the first row), and the response
-- showed the first account's id. Existing duplicates keep their rows, and all
-- but the oldest get the start of their id appended, so the index can be
-- created; an admin can rename them afterwards.
UPDATE user
SET username = username || '-' || substr(id, 1, 8)
WHERE rowid NOT IN (SELECT MIN(rowid) FROM user GROUP BY username);

CREATE UNIQUE INDEX user_username_unique ON user (username);
