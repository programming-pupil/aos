-- Freeze the executable contract and input identity at authorization time so
-- approval resume and terminal transitions cannot drift across registry reloads.
ALTER TABLE tool_invocations ADD COLUMN input_hash TEXT;
ALTER TABLE tool_invocations ADD COLUMN contract_json TEXT;
