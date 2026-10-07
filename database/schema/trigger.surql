-- Source triggers (docs/adr/043): what the daemon remembers so an asynchronous delivery is restart-safe and
-- idempotent. Applied through SurrealKit's `Sync`, like every other file here (docs/adr/004); purely additive, so no
-- data migration.
--
-- A trigger's *definition* is not here: it lives in the configuration file as `[[triggers]]`, and so does whether it
-- is enabled. Nothing in these tables can switch a trigger on, which is what keeps a restart from overriding a user's
-- choice. They hold only progress: when it last fired, when it is next due, the last failure, and the deliveries
-- accepted from a webhook.

-- One row per trigger that has done anything. The record id is the trigger's name, so no unique index is needed.
-- A row for a trigger that was since removed from the file is harmless and is deleted when the removal goes through
-- the daemon.
DEFINE TABLE IF NOT EXISTS trigger_state SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS name ON TABLE trigger_state TYPE string;
-- `domain::TriggerState` as JSON: counters, timestamps and the last error. `FLEXIBLE`: see the comment on
-- `drawer.source` in palace.surql.
DEFINE FIELD IF NOT EXISTS state ON TABLE trigger_state TYPE object FLEXIBLE;
DEFINE FIELD IF NOT EXISTS updated_at ON TABLE trigger_state TYPE datetime;

-- One row per accepted webhook delivery that carried an id. The record id is derived from the trigger and the
-- sender's id, so accepting the same delivery twice is a conflict on creation, which is how a repeat is recognised.
DEFINE TABLE IF NOT EXISTS trigger_delivery SCHEMAFULL;
DEFINE FIELD IF NOT EXISTS trigger ON TABLE trigger_delivery TYPE string;
DEFINE FIELD IF NOT EXISTS key ON TABLE trigger_delivery TYPE string;
DEFINE FIELD IF NOT EXISTS received_at ON TABLE trigger_delivery TYPE datetime;
-- The job it became or was merged into. Unset between being accepted and being queued, which is the window a crash
-- could lose it in: `recover_triggers` queues what it finds here without a job.
DEFINE FIELD IF NOT EXISTS job ON TABLE trigger_delivery TYPE option<string>;
-- Pruned by age, so the oldest are found without a scan.
DEFINE INDEX IF NOT EXISTS trigger_delivery_received_idx ON TABLE trigger_delivery FIELDS received_at;
