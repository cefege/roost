-- Prompts an account submitted in one worker's working directory, shared by
-- every device and viewer of that account. `seq` orders entries; the store
-- keeps the newest PROMPT_HISTORY_LIMIT per scope.
CREATE TABLE agent_prompt_history (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  account_id TEXT NOT NULL REFERENCES accounts(id),
  worker_fp TEXT NOT NULL,
  cwd TEXT NOT NULL,
  text TEXT NOT NULL,
  created_ms INTEGER NOT NULL
);

CREATE INDEX agent_prompt_history_scope ON agent_prompt_history (account_id, worker_fp, cwd, seq);
