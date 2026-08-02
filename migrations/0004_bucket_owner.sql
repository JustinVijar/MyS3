-- Per-bucket owner account. Only this account may delete the bucket.

ALTER TABLE bucket ADD COLUMN owner_account_id INTEGER
  REFERENCES account(id) ON DELETE SET NULL;

UPDATE bucket
SET owner_account_id = (
  SELECT a.id FROM account a
  INNER JOIN account_role ar ON ar.account_id = a.id
  INNER JOIN role r ON r.id = ar.role_id
  WHERE r.is_owner = 1
  ORDER BY a.id ASC
  LIMIT 1
)
WHERE owner_account_id IS NULL;

CREATE INDEX IF NOT EXISTS idx_bucket_owner ON bucket(owner_account_id);
