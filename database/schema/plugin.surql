-- One row per installed plugin. Module state remains separate: source_package owns source activation, and integration
-- receipts own agent installation. The generation is immutable, so a failed update can leave the previous bytes live.
DEFINE TABLE IF NOT EXISTS plugin SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name ON TABLE plugin TYPE string;
DEFINE FIELD IF NOT EXISTS manifest ON TABLE plugin TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS digest ON TABLE plugin TYPE string;
DEFINE FIELD IF NOT EXISTS generation ON TABLE plugin TYPE string;
DEFINE FIELD IF NOT EXISTS upstream ON TABLE plugin TYPE option<string>;
DEFINE FIELD IF NOT EXISTS signed_by ON TABLE plugin TYPE option<string>;
DEFINE FIELD IF NOT EXISTS installed_at ON TABLE plugin TYPE datetime;
DEFINE FIELD IF NOT EXISTS updated_at ON TABLE plugin TYPE datetime;
