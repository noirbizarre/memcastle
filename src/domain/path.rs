//! `wing`, `wing/room` and `wing/room/drawer` paths, and the names they are made of.
//!
//! Pure parsing and validation, so the CLI can reject a malformed path before
//! it ever contacts the daemon, and the daemon applies the very same rules to a
//! REST caller.

use std::fmt;

use crate::error::{Error, Result};

/// What a name labels, for error messages and for the rules that differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    /// A wing's name: one path segment.
    Wing,
    /// A room's name: one path segment.
    Room,
    /// A drawer's name: may itself contain `/` (`files/src/main.rs`).
    Drawer,
}

impl fmt::Display for NameKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Wing => "wing",
            Self::Room => "room",
            Self::Drawer => "drawer",
        })
    }
}

/// Check that `name` can label a `kind`.
///
/// A wing or room name is one path segment, so it cannot contain `/`. A drawer
/// name may (mining names a drawer after its file path) but no segment may be
/// empty or a `.`/`..` step. None may look like a UUID: a UUID is how a record
/// is addressed when it has no usable name, so a name that parsed as one would
/// make `wing/room/<uuid>` ambiguous.
///
/// # Errors
///
/// [`Error::InvalidPalacePath`] naming what is wrong with `name`.
pub fn validate_name(kind: NameKind, name: &str) -> Result<()> {
    let invalid = |message: &str| {
        Err(Error::invalid_palace_path(
            name,
            format!("{kind} name {message}"),
        ))
    };
    if name.is_empty() {
        return invalid("must not be empty");
    }
    if name.trim() != name {
        return invalid("must not start or end with whitespace");
    }
    if name.chars().any(char::is_control) {
        return invalid("must not contain control characters");
    }
    if uuid::Uuid::parse_str(name).is_ok() {
        return invalid("must not look like a UUID, which is reserved for addressing by id");
    }
    match kind {
        NameKind::Wing | NameKind::Room if name.contains('/') => {
            return invalid("must not contain `/`");
        }
        NameKind::Drawer
            if name
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..") =>
        {
            return invalid("must not have an empty, `.` or `..` segment");
        }
        _ => {}
    }
    Ok(())
}

/// A location in the palace: a wing, optionally a room in it, optionally a
/// drawer in that. Each part is a name or an id; resolving which is the
/// store's business, not the parser's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PalacePath {
    /// The wing (name or id).
    pub wing: String,
    /// The room (name or id), when the path goes that deep.
    pub room: Option<String>,
    /// The drawer (name or id), when the path goes that deep.
    pub drawer: Option<String>,
}

impl PalacePath {
    /// Split `raw` into at most three parts. Everything after the second `/`
    /// is the drawer, so a drawer name may contain slashes.
    ///
    /// Parts are only checked for being present: whether a part is a *usable
    /// new name* is [`validate_name`]'s job, applied by the commands that
    /// create something, so that an existing record with an odd name can still
    /// be shown or deleted.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPalacePath`] if a part is empty or the path has a
    /// leading or trailing `/`.
    pub fn parse(raw: &str) -> Result<Self> {
        let mut parts = raw.splitn(3, '/');
        let wing = parts.next().unwrap_or_default();
        let room = parts.next();
        let drawer = parts.next();
        for part in [Some(wing), room, drawer].into_iter().flatten() {
            if part.is_empty() {
                return Err(Error::invalid_palace_path(
                    raw,
                    "a part of the path is empty",
                ));
            }
        }
        Ok(Self {
            wing: wing.to_string(),
            room: room.map(str::to_string),
            drawer: drawer.map(str::to_string),
        })
    }

    /// Parse `raw`, requiring it to name exactly a wing.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPalacePath`] if it is not a bare wing name.
    pub fn parse_wing(raw: &str) -> Result<String> {
        let path = Self::parse(raw)?;
        if path.room.is_some() {
            return Err(Error::invalid_palace_path(
                raw,
                "expected a wing name, not a path",
            ));
        }
        Ok(path.wing)
    }

    /// Parse `raw`, requiring it to name exactly `wing/room`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPalacePath`] if it is not `<wing>/<room>`.
    pub fn parse_room(raw: &str) -> Result<(String, String)> {
        let path = Self::parse(raw)?;
        match (path.room, path.drawer) {
            (Some(room), None) => Ok((path.wing, room)),
            _ => Err(Error::invalid_palace_path(raw, "expected `<wing>/<room>`")),
        }
    }

    /// Parse `raw`, requiring it to name `wing/room/drawer`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPalacePath`] if it is not `<wing>/<room>/<drawer>`.
    pub fn parse_drawer(raw: &str) -> Result<(String, String, String)> {
        let path = Self::parse(raw)?;
        match (path.room, path.drawer) {
            (Some(room), Some(drawer)) => Ok((path.wing, room, drawer)),
            _ => Err(Error::invalid_palace_path(
                raw,
                "expected `<wing>/<room>/<drawer>`",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_splits_into_wing_room_and_drawer() {
        let path = PalacePath::parse("work/project-x/context").unwrap();
        assert_eq!(path.wing, "work");
        assert_eq!(path.room.as_deref(), Some("project-x"));
        assert_eq!(path.drawer.as_deref(), Some("context"));
    }

    #[test]
    fn everything_after_the_second_slash_is_the_drawer_name() {
        let (_, _, drawer) = PalacePath::parse_drawer("w/files/src/main.rs").unwrap();
        assert_eq!(drawer, "src/main.rs");
    }

    #[test]
    fn a_shorter_path_leaves_the_deeper_parts_empty() {
        assert_eq!(PalacePath::parse_wing("work").unwrap(), "work");
        assert_eq!(
            PalacePath::parse_room("work/x").unwrap(),
            ("work".to_string(), "x".to_string())
        );
    }

    #[test]
    fn a_path_with_an_empty_part_is_refused() {
        for raw in ["", "/", "work/", "/room", "work//drawer", "work/room/"] {
            assert!(PalacePath::parse(raw).is_err(), "{raw:?} should be refused");
        }
    }

    #[test]
    fn a_path_of_the_wrong_depth_is_refused_by_the_depth_specific_parsers() {
        assert!(PalacePath::parse_wing("work/x").is_err());
        assert!(PalacePath::parse_room("work").is_err());
        assert!(PalacePath::parse_room("work/x/y").is_err());
        assert!(PalacePath::parse_drawer("work/x").is_err());
    }

    #[test]
    fn a_wing_or_room_name_cannot_contain_a_slash_but_a_drawer_name_can() {
        assert!(validate_name(NameKind::Wing, "a/b").is_err());
        assert!(validate_name(NameKind::Room, "a/b").is_err());
        assert!(validate_name(NameKind::Drawer, "a/b").is_ok());
    }

    #[test]
    fn a_drawer_name_cannot_climb_or_leave_a_gap() {
        for name in ["../x", "a/./b", "a//b", "/a", "a/"] {
            assert!(
                validate_name(NameKind::Drawer, name).is_err(),
                "{name:?} should be refused"
            );
        }
    }

    #[test]
    fn a_name_that_looks_like_a_uuid_is_refused_so_ids_stay_unambiguous() {
        let id = uuid::Uuid::new_v4().to_string();
        for kind in [NameKind::Wing, NameKind::Room, NameKind::Drawer] {
            assert!(validate_name(kind, &id).is_err());
        }
    }

    #[test]
    fn blank_padded_or_control_character_names_are_refused() {
        for name in ["", " x", "x ", "a\nb"] {
            assert!(validate_name(NameKind::Wing, name).is_err(), "{name:?}");
        }
    }
}
