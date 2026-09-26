//! Typed record identifiers.
//!
//! Every id is a UUID generated in application code, not left to the store
//! to assign — that way a `Job`/`Drawer`/etc. has a stable identity before
//! it is ever persisted, and the domain layer never has to ask `store` "what
//! id did you give this?".

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
