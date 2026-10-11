-- A source name that once belonged to a plugin keeps its owner after uninstall; data and cursor are intentionally
-- retained, and a different publisher must not inherit them by installing a namesake.
DEFINE TABLE IF NOT EXISTS plugin_source_owner SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS source ON TABLE plugin_source_owner TYPE string;
DEFINE FIELD IF NOT EXISTS plugin ON TABLE plugin_source_owner TYPE string;
