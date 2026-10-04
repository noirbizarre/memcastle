//! Typed record identifiers.
//!
//! Every id is a UUID generated in application code, not left to the store
//! to assign — that way a `Job`/`Drawer`/etc. has a stable identity before
//! it is ever persisted, and the domain layer never has to ask `store` "what
//! id did you give this?".
//!
//! **Who assigns ids and timestamps:** the caller does, for every plain
//! create/update (`create_drawer`, `create_relationship`,
//! `supersede_relationship`, `invalidate_relationship`, `save_job`) — so a
//! job handler can derive an id from (job, item index) and replay safely, and
//! a test can pin a time. The one exception is `get_or_create_*`
//! (palace, wing, room, entity), where only the store knows whether a create
//! will happen at all, so it mints the id and creation time when it does.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

// One macro definition instead of six near-identical structs — the
// boilerplate is identical for every id, so a hand-written copy per type
// would just be six places a typo could hide.
macro_rules! define_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generate a fresh, random identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Derive the same identifier every time from `seed` and
            /// `name`, instead of generating a random one.
            ///
            /// For writes a job may replay after a crash: the first attempt
            /// may have stored the record but died before checkpointing, and
            /// a random id on the replay would store it a second time. With
            /// an id derived from (job, item index), the replay names the
            /// record it already wrote and can recognise it as done.
            ///
            /// SHA-256 over `seed || name`, truncated, with the UUID
            /// version/variant bits set so it round-trips through
            /// `Uuid::parse_str` like any other id. (Not RFC 4122's SHA-1
            /// v5 — nothing outside this process needs to reproduce it.)
            #[must_use]
            pub fn derive(seed: Uuid, name: &str) -> Self {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(seed.as_bytes());
                hasher.update(name.as_bytes());
                let digest = hasher.finalize();
                let mut bytes = [0u8; 16];
                bytes.copy_from_slice(&digest[..16]);
                bytes[6] = (bytes[6] & 0x0f) | 0x50;
                bytes[8] = (bytes[8] & 0x3f) | 0x80;
                Self(Uuid::from_bytes(bytes))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }

        // Hand-written rather than `#[serde(transparent)]` deriving through
        // to `Uuid`'s own impl: `Uuid`'s `Deserialize` switches to expecting
        // a 16-byte array for non-human-readable formats, and SurrealDB's
        // response deserializer reports itself as non-human-readable even
        // though every id crosses the wire as plain text here (see
        // `store::mod`'s cast convention) — going through `String`
        // explicitly sidesteps that switch entirely, for every format.
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                raw.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

define_id!(
    /// Identifies a [`Palace`](super::Palace).
    PalaceId
);
define_id!(
    /// Identifies a [`Wing`](super::Wing).
    WingId
);
define_id!(
    /// Identifies a [`Room`](super::Room).
    RoomId
);
define_id!(
    /// Identifies a [`Drawer`](super::Drawer).
    DrawerId
);
define_id!(
    /// Identifies a [`Job`](super::Job).
    JobId
);
define_id!(
    /// Identifies an [`Entity`](super::Entity).
    EntityId
);
define_id!(
    /// Identifies a [`Relationship`](super::Relationship).
    RelationshipId
);
define_id!(
    /// Identifies a mining source (see [`SourceRef`](super::SourceRef)); derived from what the
    /// source *is*, never random, so two jobs mining the same place agree on it.
    SourceId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_derived_id_is_the_same_every_time_for_the_same_inputs() {
        let seed = Uuid::new_v4();
        assert_eq!(
            DrawerId::derive(seed, "item:3"),
            DrawerId::derive(seed, "item:3")
        );
    }

    #[test]
    fn derived_ids_differ_by_seed_and_by_name() {
        let seed = Uuid::new_v4();
        assert_ne!(
            DrawerId::derive(seed, "item:3"),
            DrawerId::derive(seed, "item:4")
        );
        assert_ne!(
            DrawerId::derive(seed, "item:3"),
            DrawerId::derive(Uuid::new_v4(), "item:3")
        );
    }

    #[test]
    fn a_derived_id_survives_the_string_round_trip_every_store_write_relies_on() {
        let id = DrawerId::derive(Uuid::new_v4(), "item:0");
        assert_eq!(id.to_string().parse::<DrawerId>().unwrap(), id);
        assert_eq!(id.0.get_version_num(), 5);
    }
}
