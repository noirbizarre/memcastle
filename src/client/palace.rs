//! The client side of the hierarchy routes: wings, rooms and drawers.

use serde_json::json;

use crate::app::{Created, EntityLink, Superseded, WingDetail};
use crate::domain::channel::CLI as CHANNEL;
use crate::domain::{Deleted, Drawer, DrawerHistory, DrawerSummary, RoomSummary, WingSummary};
use crate::error::{Error, Result};

use super::DaemonClient;

impl DaemonClient {
    /// The URL of `/api/wings/...`, with every segment percent-encoded.
    ///
    /// Segments are pushed one by one instead of formatted into a string: a
    /// name with a space, `?` or `#` in it would otherwise end the path early
    /// and address something else. `trailing` is a drawer name, whose own `/`
    /// are kept as path separators because the daemon's drawer segment is a
    /// wildcard.
    fn palace_url(&self, segments: &[&str], trailing: Option<&str>) -> Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(&self.base_url).map_err(|source| Error::Client {
            message: format!("invalid daemon address `{}`: {source}", self.base_url),
        })?;
        let mut path = url.path_segments_mut().map_err(|()| Error::Client {
            message: format!("invalid daemon address `{}`", self.base_url),
        })?;
        path.pop_if_empty().push("api").push("wings");
        for segment in segments {
            path.push(segment);
        }
        if let Some(trailing) = trailing {
            for segment in trailing.split('/') {
                path.push(segment);
            }
        }
        drop(path);
        Ok(url)
    }

    /// Every wing with its counts (`GET /api/wings`).
    ///
    /// # Errors
    ///
    /// [`Error::DaemonNotRunning`], or [`Error::Remote`] if the daemon refuses.
    pub async fn list_wings(&self) -> Result<Vec<WingSummary>> {
        self.send(self.http.get(self.palace_url(&[], None)?)).await
    }

    /// One wing with its rooms (`GET /api/wings/{wing}`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_wings`]; a missing wing is a 404 [`Error::Remote`].
    pub async fn show_wing(&self, wing: &str) -> Result<WingDetail> {
        self.send(self.http.get(self.palace_url(&[wing], None)?))
            .await
    }

    /// Create a wing, or find it (`POST /api/wings`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_wings`].
    pub async fn create_wing(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> Result<Created<WingSummary>> {
        self.send(
            self.http
                .post(self.palace_url(&[], None)?)
                .json(&json!({ "name": name, "description": description })),
        )
        .await
    }

    /// Delete a wing and everything in it (`DELETE /api/wings/{wing}`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_wings`]; a pending mining or checkpoint job is a
    /// 409 [`Error::Remote`].
    pub async fn delete_wing(&self, wing: &str) -> Result<Deleted> {
        self.send(self.http.delete(self.palace_url(&[wing], None)?))
            .await
    }

    /// The rooms of a wing (`GET /api/wings/{wing}/rooms`).
    ///
    /// # Errors
    ///
    /// As for [`Self::show_wing`].
    pub async fn list_rooms(&self, wing: &str) -> Result<Vec<RoomSummary>> {
        self.send(self.http.get(self.palace_url(&[wing, "rooms"], None)?))
            .await
    }

    /// One room (`GET /api/wings/{wing}/rooms/{room}`).
    ///
    /// # Errors
    ///
    /// As for [`Self::show_wing`].
    pub async fn show_room(&self, wing: &str, room: &str) -> Result<RoomSummary> {
        self.send(
            self.http
                .get(self.palace_url(&[wing, "rooms", room], None)?),
        )
        .await
    }

    /// Create a room, or find it (`POST /api/wings/{wing}/rooms`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_wings`].
    pub async fn create_room(
        &self,
        wing: &str,
        name: &str,
        description: Option<&str>,
    ) -> Result<Created<RoomSummary>> {
        self.send(
            self.http
                .post(self.palace_url(&[wing, "rooms"], None)?)
                .json(&json!({ "name": name, "description": description })),
        )
        .await
    }

    /// Delete a room and its drawers (`DELETE /api/wings/{wing}/rooms/{room}`).
    ///
    /// # Errors
    ///
    /// As for [`Self::delete_wing`].
    pub async fn delete_room(&self, wing: &str, room: &str) -> Result<Deleted> {
        self.send(
            self.http
                .delete(self.palace_url(&[wing, "rooms", room], None)?),
        )
        .await
    }

    /// The newest drawers of a room as listing rows
    /// (`GET /api/wings/{wing}/rooms/{room}/drawers`).
    ///
    /// # Errors
    ///
    /// As for [`Self::show_wing`].
    pub async fn list_drawers(
        &self,
        wing: &str,
        room: &str,
        limit: Option<u32>,
    ) -> Result<Vec<DrawerSummary>> {
        let mut request = self
            .http
            .get(self.palace_url(&[wing, "rooms", room, "drawers"], None)?);
        // Only appended when set: an absent parameter is what means "the
        // daemon's default limit".
        if let Some(limit) = limit {
            request = request.query(&[("limit", limit)]);
        }
        self.send(request).await
    }

    /// One drawer in full, by name or id
    /// (`GET /api/wings/{wing}/rooms/{room}/drawers/{drawer}`).
    ///
    /// # Errors
    ///
    /// As for [`Self::show_wing`].
    pub async fn show_drawer(&self, wing: &str, room: &str, drawer: &str) -> Result<Drawer> {
        self.send(
            self.http
                .get(self.palace_url(&[wing, "rooms", room, "drawers"], Some(drawer))?),
        )
        .await
    }

    /// Write a drawer, optionally named
    /// (`POST /api/wings/{wing}/rooms/{room}/drawers`).
    ///
    /// # Errors
    ///
    /// As for [`Self::list_wings`]; a name held by other content is a 409
    /// [`Error::Remote`].
    pub async fn create_drawer(
        &self,
        wing: &str,
        room: &str,
        name: Option<&str>,
        content: String,
    ) -> Result<Created<Drawer>> {
        self.send(
            self.http
                .post(self.palace_url(&[wing, "rooms", room, "drawers"], None)?)
                .json(&json!({
                    "name": name,
                    "content": content,
                    "requested_by": CHANNEL,
                })),
        )
        .await
    }

    /// Capture a note in `wing`/`room` (`POST /api/notes`), recording `uri` as where it was captured.
    ///
    /// # Errors
    ///
    /// As for [`Self::list_wings`]; blank content or an unusable wing or room name is a 400
    /// [`Error::Remote`].
    pub async fn write_note(
        &self,
        wing: &str,
        room: &str,
        content: String,
        uri: Option<&str>,
    ) -> Result<Created<Drawer>> {
        self.send(
            self.http
                .post(format!("{}/api/notes", self.base_url))
                .json(&json!({
                    "wing": wing,
                    "room": room,
                    "content": content,
                    "uri": uri,
                    "requested_by": CHANNEL,
                })),
        )
        .await
    }

    /// End a drawer's validity and optionally open a replacement
    /// (`POST /api/drawers/{id}/supersede`).
    ///
    /// # Errors
    ///
    /// As for [`Self::show_wing`]; a drawer that already ended is a 409.
    pub async fn supersede_drawer(&self, id: &str, content: Option<String>) -> Result<Superseded> {
        self.send(
            self.http
                .post(format!("{}/api/drawers/{id}/supersede", self.base_url))
                .json(&json!({ "content": content, "requested_by": CHANNEL })),
        )
        .await
    }

    /// Every version of the knowledge a drawer belongs to, oldest first
    /// (`GET /api/drawers/{id}/history`).
    ///
    /// # Errors
    ///
    /// As for [`Self::show_wing`].
    pub async fn drawer_history(&self, id: &str) -> Result<DrawerHistory> {
        self.send(
            self.http
                .get(format!("{}/api/drawers/{id}/history", self.base_url)),
        )
        .await
    }

    /// Record that a drawer mentions an entity
    /// (`POST /api/drawers/{id}/mentions`).
    ///
    /// # Errors
    ///
    /// As for [`Self::show_wing`].
    pub async fn link_drawer_entity(&self, id: &str, name: &str, kind: &str) -> Result<EntityLink> {
        self.send(
            self.http
                .post(format!("{}/api/drawers/{id}/mentions", self.base_url))
                .json(&json!({ "name": name, "kind": kind })),
        )
        .await
    }

    /// Delete one drawer
    /// (`DELETE /api/wings/{wing}/rooms/{room}/drawers/{drawer}`).
    ///
    /// # Errors
    ///
    /// As for [`Self::show_wing`].
    pub async fn delete_drawer(&self, wing: &str, room: &str, drawer: &str) -> Result<Deleted> {
        self.send(
            self.http
                .delete(self.palace_url(&[wing, "rooms", room, "drawers"], Some(drawer))?),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> DaemonClient {
        // No registry entry for this path, so the configured address is used.
        DaemonClient::discover(
            std::path::Path::new("/nonexistent/palace"),
            "127.0.0.1:8420".parse().unwrap(),
        )
    }

    #[test]
    fn a_name_with_reserved_characters_cannot_end_the_path_early() {
        let url = client().palace_url(&["my wing", "a?b#c"], None).unwrap();
        assert_eq!(url.path(), "/api/wings/my%20wing/a%3Fb%23c");
        assert!(url.query().is_none() && url.fragment().is_none());
    }

    #[test]
    fn a_drawer_name_keeps_its_slashes_as_path_separators() {
        let url = client()
            .palace_url(&["w", "rooms", "r", "drawers"], Some("src/lib.rs"))
            .unwrap();
        assert_eq!(url.path(), "/api/wings/w/rooms/r/drawers/src/lib.rs");
    }
}
