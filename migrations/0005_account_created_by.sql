-- Track who created an account so creators can delete those accounts.

ALTER TABLE account ADD COLUMN created_by_account_id INTEGER
  REFERENCES account(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_account_created_by ON account(created_by_account_id);
