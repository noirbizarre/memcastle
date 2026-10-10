-- Installed mining sources (docs/adr/026): the WebAssembly packages a user added, and where each is in its
-- lifecycle. Applied through SurrealKit's `Sync`, like every other file here (docs/adr/004); purely additive, so no
-- data migration.
--
-- Only the lifecycle and what the user agreed to live here. The component itself is a file under the sources
-- directory, and `digest` is what ties the two together: a file that no longer matches is `unavailable` and is
-- never run. `unavailable` is computed on every look, so it is deliberately not a stored state.

-- One row per installed source. The record id is the source's name, so installing the same name twice is the
-- same row and no unique index is needed.
DEFINE TABLE IF NOT EXISTS source_package SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name ON TABLE source_package TYPE string;
-- `installed`, `enabled` or `disabled` (`domain::SourcePackageState`).
DEFINE FIELD IF NOT EXISTS state ON TABLE source_package TYPE string;
-- SHA-256 of the installed component.
DEFINE FIELD IF NOT EXISTS digest ON TABLE source_package TYPE string;
-- The manifest as installed, permissions included: the daemon runs on this copy, never on the file beside the
-- component. `FLEXIBLE`: see the comment on `drawer.source` in palace.surql.
DEFINE FIELD IF NOT EXISTS manifest ON TABLE source_package TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS installed_at ON TABLE source_package TYPE datetime;
DEFINE FIELD IF NOT EXISTS updated_at ON TABLE source_package TYPE datetime;

-- Where the package came from (docs/adr/033), so `update` knows which index to ask. All optional and additive: a row
-- written before registries existed has none of them and is read as a package installed from disk.
-- `package`, `bundled` or `registry` (`domain::SourceOrigin`).
DEFINE FIELD IF NOT EXISTS origin ON TABLE source_package TYPE option<string>;
-- The index location a bundled or registry package was installed from.
DEFINE FIELD IF NOT EXISTS registry ON TABLE source_package TYPE option<string>;
-- SHA-256 of the archive it was installed from: what the index published and the install verified.
DEFINE FIELD IF NOT EXISTS archive_digest ON TABLE source_package TYPE option<string>;
-- The id of the key whose signature on the archive was verified at install.
DEFINE FIELD IF NOT EXISTS signed_by ON TABLE source_package TYPE option<string>;
-- Optional ownership of a plugin source module. Existing format-1 sources have neither field.
DEFINE FIELD IF NOT EXISTS plugin ON TABLE source_package TYPE option<string>;
DEFINE FIELD IF NOT EXISTS generation ON TABLE source_package TYPE option<string>;
