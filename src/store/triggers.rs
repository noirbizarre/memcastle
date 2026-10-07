//! Trigger repository methods: the `trigger_state` and `trigger_delivery` tables (docs/adr/043).
//!
//! Progress only. A trigger's definition and whether it is enabled live in the configuration file, so nothing stored
//! here can switch a trigger on.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::domain::{TriggerDelivery, TriggerState, sha256_hex};
use crate::error::{Error, Result};

use super::SurrealStore;

/// The record id of a delivery: derived from what identifies it, so the same delivery is the same record.
fn delivery_id(trigger: &str, key: &str) -> String {
    // A NUL cannot appear in a header value, so two different (trigger, key) pairs never join to the same text.
    sha256_hex(format!("{trigger}\0{key}").as_bytes())
}

#[derive(Deserialize)]
struct StateRow {
    state: Value,
}

#[derive(Deserialize)]
struct DeliveryRow {
    trigger: String,
    key: String,
    received_at: DateTime<Utc>,
    #[serde(default)]
    job: Option<String>,
}

impl DeliveryRow {
    fn into_delivery(self) -> Result<TriggerDelivery> {
        let job = self
            .job
            .map(|raw| raw.parse())
            .transpose()
            .map_err(|e| Error::store_malformed(format!("trigger_delivery job: {e}")))?;
        Ok(TriggerDelivery {
            trigger: self.trigger,
            key: self.key,
            received_at: self.received_at,
            job,
        })
    }
}

const DELIVERY_COLUMNS: &str = "trigger, key, <string>received_at AS received_at, job";

impl SurrealStore {
    /// What the daemon remembers about trigger `name`, or `None` when it never did anything.
    pub async fn get_trigger_state(&self, name: &str) -> Result<Option<TriggerState>> {
        let mut response = self
            .db
            .query(
                "SELECT state FROM trigger_state WHERE id = type::record('trigger_state', $name)",
            )
            .bind(("name", name.to_string()))
            .await?;
        let mut rows: Vec<StateRow> = super::take_rows(&mut response, 0)?;
        rows.pop()
            .map(|row| {
                serde_json::from_value(row.state)
                    .map_err(|e| Error::store_malformed(format!("trigger_state: {e}")))
            })
            .transpose()
    }

    /// Every remembered trigger state, by name.
    pub async fn list_trigger_states(&self) -> Result<Vec<TriggerState>> {
        let mut response = self
            .db
            .query("SELECT state FROM trigger_state ORDER BY name")
            .await?;
        let rows: Vec<StateRow> = super::take_rows(&mut response, 0)?;
        rows.into_iter()
            .map(|row| {
                serde_json::from_value(row.state)
                    .map_err(|e| Error::store_malformed(format!("trigger_state: {e}")))
            })
            .collect()
    }

    /// Store `state`, replacing what was there.
    pub async fn save_trigger_state(&self, state: &TriggerState) -> Result<()> {
        self.db
            .query(
                "UPSERT type::record('trigger_state', $name) SET \
                 name = $name, state = $state, updated_at = <datetime>$at",
            )
            .bind(("name", state.name.clone()))
            .bind(("state", super::bindable(state)?))
            .bind(("at", super::stored(Utc::now())))
            .await?
            .check()?;
        Ok(())
    }

    /// Forget trigger `name`: its state and the deliveries it accepted. Called when the trigger is removed, so a new
    /// trigger of the same name does not inherit another's history.
    pub async fn delete_trigger_records(&self, name: &str) -> Result<()> {
        self.db
            .query(
                "DELETE type::record('trigger_state', $name); \
                 DELETE trigger_delivery WHERE trigger = $name;",
            )
            .bind(("name", name.to_string()))
            .await?
            .check()?;
        Ok(())
    }

    /// Record a delivery, once.
    ///
    /// Returns `false` when this trigger already accepted a delivery with this key, which is how a sender's repeat is
    /// recognised. The creation is the check, so two daemons sharing a remote palace cannot both accept it.
    pub async fn create_trigger_delivery_once(&self, delivery: &TriggerDelivery) -> Result<bool> {
        let created = self
            .db
            .query(
                "CREATE type::record('trigger_delivery', $id) SET \
                 trigger = $trigger, key = $key, received_at = <datetime>$received_at, job = NONE",
            )
            .bind(("id", delivery_id(&delivery.trigger, &delivery.key)))
            .bind(("trigger", delivery.trigger.clone()))
            .bind(("key", delivery.key.clone()))
            .bind(("received_at", super::stored(delivery.received_at)))
            .await?
            .check();
        match created {
            Ok(_) => Ok(true),
            // Whatever the reason, a record that is there is a delivery that was accepted; one that is not is a real
            // failure to report.
            Err(error) => {
                if self
                    .get_trigger_delivery(&delivery.trigger, &delivery.key)
                    .await?
                    .is_some()
                {
                    Ok(false)
                } else {
                    Err(error.into())
                }
            }
        }
    }

    /// The delivery of `trigger` with `key`, or `None`.
    pub async fn get_trigger_delivery(
        &self,
        trigger: &str,
        key: &str,
    ) -> Result<Option<TriggerDelivery>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {DELIVERY_COLUMNS} FROM trigger_delivery WHERE id = type::record('trigger_delivery', $id)"
            ))
            .bind(("id", delivery_id(trigger, key)))
            .await?;
        let mut rows: Vec<DeliveryRow> = super::take_rows(&mut response, 0)?;
        rows.pop().map(DeliveryRow::into_delivery).transpose()
    }

    /// Forget one delivery, so a retry of it is a new attempt.
    pub async fn delete_trigger_delivery(&self, trigger: &str, key: &str) -> Result<()> {
        self.db
            .query("DELETE type::record('trigger_delivery', $id)")
            .bind(("id", delivery_id(trigger, key)))
            .await?
            .check()?;
        Ok(())
    }

    /// Say which job a delivery became.
    pub async fn set_trigger_delivery_job(
        &self,
        trigger: &str,
        key: &str,
        job: crate::domain::JobId,
    ) -> Result<()> {
        self.db
            .query("UPDATE type::record('trigger_delivery', $id) SET job = $job")
            .bind(("id", delivery_id(trigger, key)))
            .bind(("job", job.to_string()))
            .await?
            .check()?;
        Ok(())
    }

    /// Deliveries that were accepted but never queued: what a crash between the two leaves behind.
    pub async fn list_unqueued_trigger_deliveries(&self) -> Result<Vec<TriggerDelivery>> {
        let mut response = self
            .db
            .query(format!(
                "SELECT {DELIVERY_COLUMNS} FROM trigger_delivery WHERE job = NONE ORDER BY received_at"
            ))
            .await?;
        let rows: Vec<DeliveryRow> = super::take_rows(&mut response, 0)?;
        rows.into_iter().map(DeliveryRow::into_delivery).collect()
    }

    /// Forget deliveries accepted before `before`: a sender's retries stop long before, and the table must not grow
    /// without bound.
    pub async fn prune_trigger_deliveries(&self, before: DateTime<Utc>) -> Result<()> {
        self.db
            .query("DELETE trigger_delivery WHERE received_at < <datetime>$before AND job != NONE")
            .bind(("before", super::stored(before)))
            .await?
            .check()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;
    use crate::domain::JobId;

    fn delivery(trigger: &str, key: &str, at: DateTime<Utc>) -> TriggerDelivery {
        TriggerDelivery {
            trigger: trigger.to_string(),
            key: key.to_string(),
            received_at: at,
            job: None,
        }
    }

    #[tokio::test]
    async fn a_trigger_state_is_stored_and_read_back_whole() {
        let store = SurrealStore::connect_memory_for_tests().await;
        assert!(store.get_trigger_state("daily").await.unwrap().is_none());
        let mut state = TriggerState::new("daily");
        state.fired = 3;
        state.next_due = Some(Utc::now());
        state.failed("no route", Utc::now());
        store.save_trigger_state(&state).await.unwrap();
        assert_eq!(
            store.get_trigger_state("daily").await.unwrap(),
            Some(state.clone())
        );
        assert_eq!(store.list_trigger_states().await.unwrap(), vec![state]);
    }

    #[tokio::test]
    async fn the_same_delivery_is_accepted_once_and_only_by_the_trigger_it_came_to() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let now = Utc::now();
        assert!(
            store
                .create_trigger_delivery_once(&delivery("a", "d-1", now))
                .await
                .unwrap()
        );
        assert!(
            !store
                .create_trigger_delivery_once(&delivery("a", "d-1", now))
                .await
                .unwrap()
        );
        assert!(
            store
                .create_trigger_delivery_once(&delivery("b", "d-1", now))
                .await
                .unwrap()
        );
        assert!(
            store
                .create_trigger_delivery_once(&delivery("a", "d-2", now))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_delivery_that_was_accepted_but_never_queued_is_found_until_it_has_a_job() {
        let store = SurrealStore::connect_memory_for_tests().await;
        store
            .create_trigger_delivery_once(&delivery("a", "d-1", Utc::now()))
            .await
            .unwrap();
        assert_eq!(
            store
                .list_unqueued_trigger_deliveries()
                .await
                .unwrap()
                .len(),
            1
        );
        let job = JobId::new();
        store
            .set_trigger_delivery_job("a", "d-1", job)
            .await
            .unwrap();
        assert!(
            store
                .list_unqueued_trigger_deliveries()
                .await
                .unwrap()
                .is_empty()
        );
        let kept = store
            .get_trigger_delivery("a", "d-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(kept.job, Some(job));
    }

    #[tokio::test]
    async fn pruning_forgets_old_queued_deliveries_and_keeps_the_unqueued_ones() {
        let store = SurrealStore::connect_memory_for_tests().await;
        let old = Utc::now() - Duration::days(30);
        store
            .create_trigger_delivery_once(&delivery("a", "old", old))
            .await
            .unwrap();
        store
            .set_trigger_delivery_job("a", "old", JobId::new())
            .await
            .unwrap();
        store
            .create_trigger_delivery_once(&delivery("a", "stuck", old))
            .await
            .unwrap();
        store
            .prune_trigger_deliveries(Utc::now() - Duration::days(7))
            .await
            .unwrap();
        assert!(
            store
                .get_trigger_delivery("a", "old")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_trigger_delivery("a", "stuck")
                .await
                .unwrap()
                .is_some(),
            "a delivery nothing was queued for is still owed a job"
        );
    }

    #[tokio::test]
    async fn removing_a_trigger_forgets_its_state_and_deliveries() {
        let store = SurrealStore::connect_memory_for_tests().await;
        store
            .save_trigger_state(&TriggerState::new("a"))
            .await
            .unwrap();
        store
            .create_trigger_delivery_once(&delivery("a", "d", Utc::now()))
            .await
            .unwrap();
        store.delete_trigger_records("a").await.unwrap();
        assert!(store.get_trigger_state("a").await.unwrap().is_none());
        assert!(
            store
                .get_trigger_delivery("a", "d")
                .await
                .unwrap()
                .is_none()
        );
    }
}
