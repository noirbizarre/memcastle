-- The daemon's authentication state: the verifier of the one generated bearer
-- token (docs/adr/014). MemCastle-owned application state, not palace content
-- and not a package asset.
--
-- A single fixed row (`auth_state:token`) rather than one row per token: 0.1
-- has exactly one generated credential, and replacing the row is rotation
-- while deleting it is revocation. Per-credential rows arrive with the
-- multi-user model, as an additive change.
--
-- Only a digest is stored, never the token: `digest` is the SHA-256 of a
-- 256-bit random token, with `algorithm` and `version` beside it so a later
-- change of scheme is recognisable. Every read and write goes through
-- `store::auth`.
--
-- Additive and self-contained (no reference to any other table), so it needs
-- no data migration and its order relative to the other schema files never
-- matters.

DEFINE TABLE IF NOT EXISTS auth_state SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS algorithm ON TABLE auth_state TYPE string;
DEFINE FIELD IF NOT EXISTS version ON TABLE auth_state TYPE int;
DEFINE FIELD IF NOT EXISTS digest ON TABLE auth_state TYPE string;
DEFINE FIELD IF NOT EXISTS created_at ON TABLE auth_state TYPE datetime;
