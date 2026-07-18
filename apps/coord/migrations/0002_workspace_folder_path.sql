-- phase-12: workspace IS a folder. Add folder_path to workspaces.
-- Existing rows backfilled with "~" — homedir is the safest default
-- and the worker resolves ~ via os.homedir() before keeper spawn.
-- See [[project_data_model_server_workspace_pane]].

ALTER TABLE workspaces ADD COLUMN folder_path TEXT NOT NULL DEFAULT '~';
