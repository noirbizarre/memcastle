//! The palace hierarchy routes: wings, rooms and drawers.
//!
//! Wings and rooms are addressed by name or UUID, a drawer by name or UUID
//! within its room. A drawer name may contain `/`, so the drawer segment is a
//! wildcard that takes the rest of the path.
//!
//! Like the rest of the API these are guarded by the authentication layer that
//! wraps the whole router, and like `/api/auth` and `/api/db` they have no MCP
//! counterpart for the destructive half: deleting is a human decision.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::Deserialize;

use crate::domain::DrawerId;

use super::extract::{ApiJson, ApiQuery};
use super::{ApiError, ApiState, ModeHeader, default_requested_by};

/// `201 Created` when the call made the record, `200 OK` when it found it.
fn created_status(created: bool) -> StatusCode {
    if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    }
}

pub(super) async fn list_wings(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.list_wings(mode).await?))
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateWingBody {
    name: String,
    description: Option<String>,
}

pub(super) async fn create_wing(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    ApiJson(body): ApiJson<CreateWingBody>,
) -> Result<impl IntoResponse, ApiError> {
    let created = state
        .app
        .create_wing(&body.name, body.description.as_deref(), mode)
        .await?;
    Ok((created_status(created.created), Json(created)))
}

pub(super) async fn show_wing(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(wing): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.show_wing(&wing, mode).await?))
}

pub(super) async fn delete_wing(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(wing): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.delete_wing(&wing, mode).await?))
}

pub(super) async fn list_rooms(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(wing): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.list_rooms(&wing, mode).await?))
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateRoomBody {
    name: String,
    description: Option<String>,
}

pub(super) async fn create_room(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(wing): Path<String>,
    ApiJson(body): ApiJson<CreateRoomBody>,
) -> Result<impl IntoResponse, ApiError> {
    let created = state
        .app
        .create_room(&wing, &body.name, body.description.as_deref(), mode)
        .await?;
    Ok((created_status(created.created), Json(created)))
}

pub(super) async fn show_room(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path((wing, room)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.show_room(&wing, &room, mode).await?))
}

pub(super) async fn delete_room(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path((wing, room)): Path<(String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(state.app.delete_room(&wing, &room, mode).await?))
}

#[derive(Debug, Deserialize)]
pub(super) struct ListDrawersParams {
    limit: Option<u32>,
}

pub(super) async fn list_drawers(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path((wing, room)): Path<(String, String)>,
    ApiQuery(params): ApiQuery<ListDrawersParams>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .app
            .list_drawers(&wing, &room, params.limit, mode)
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
pub(super) struct CreateDrawerBody {
    /// Optional: an unnamed drawer is addressed by its UUID.
    name: Option<String>,
    content: String,
    /// The channel this write came through, `"http"` unless a caller says
    /// otherwise (the CLI sends `"cli"`).
    #[serde(default = "default_requested_by")]
    requested_by: String,
}

pub(super) async fn create_drawer(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path((wing, room)): Path<(String, String)>,
    ApiJson(body): ApiJson<CreateDrawerBody>,
) -> Result<impl IntoResponse, ApiError> {
    let created = state
        .app
        .create_drawer(
            &wing,
            &room,
            body.name.as_deref(),
            body.content,
            &body.requested_by,
            mode,
        )
        .await?;
    Ok((created_status(created.created), Json(created)))
}

#[derive(Debug, Deserialize)]
pub(super) struct WriteNoteBody {
    wing: String,
    room: String,
    content: String,
    /// Where the note was captured (the CLI sends its working directory).
    uri: Option<String>,
    /// The channel this write came through, `"http"` unless a caller says
    /// otherwise (the CLI sends `"cli"`).
    #[serde(default = "default_requested_by")]
    requested_by: String,
}

/// `POST /api/notes`: capture a note, `201` when stored and `200` when an identical one was already there.
pub(super) async fn write_note(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    ApiJson(body): ApiJson<WriteNoteBody>,
) -> Result<impl IntoResponse, ApiError> {
    let created = state
        .app
        .note_write(
            &body.wing,
            &body.room,
            body.content,
            body.uri,
            &body.requested_by,
            mode,
        )
        .await?;
    Ok((created_status(created.created), Json(created)))
}

pub(super) async fn show_drawer(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path((wing, room, drawer)): Path<(String, String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state.app.show_drawer(&wing, &room, &drawer, mode).await?,
    ))
}

pub(super) async fn delete_drawer(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path((wing, room, drawer)): Path<(String, String, String)>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state.app.delete_drawer(&wing, &room, &drawer, mode).await?,
    ))
}

/// The optional replacement a drawer is superseded by. Without `content` the
/// drawer is only invalidated.
#[derive(Debug, Deserialize)]
pub(super) struct SupersedeBody {
    /// The corrected content.
    content: Option<String>,
    /// Tags for the replacement; the old drawer's when absent.
    tags: Option<Vec<String>>,
    /// The channel this write came through.
    #[serde(default = "default_requested_by")]
    requested_by: String,
}

/// `POST /api/drawers/{id}/supersede`, by id: a search hit already carries one.
pub(super) async fn supersede_drawer(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<SupersedeBody>,
) -> Result<impl IntoResponse, ApiError> {
    let id = parse_drawer_id(&id)?;
    let replacement = body.content.map(|content| crate::app::DrawerReplacement {
        content,
        tags: body.tags,
    });
    Ok(Json(
        state
            .app
            .supersede_drawer(id, replacement, &body.requested_by, mode)
            .await?,
    ))
}

/// `GET /api/drawers/{id}/duplicates`: the drawers this one was recorded as a likely duplicate of, or that were
/// recorded as likely duplicates of it, with the evidence. Nothing here is merged.
pub(super) async fn drawer_duplicates(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id = parse_drawer_id(&id)?;
    let similar = state.app.drawer_duplicates(id, mode).await?;
    Ok(Json(
        serde_json::json!({ "drawer": id, "similar": similar }),
    ))
}

/// The body of `PUT /api/drawers/{id}/embedding`.
#[derive(Debug, Deserialize)]
pub(super) struct EmbeddingBody {
    /// The vector, of the palace's fixed dimension.
    embedding: Vec<f32>,
}

/// `PUT /api/drawers/{id}/embedding`: attach a caller-computed vector.
pub(super) async fn set_drawer_embedding(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<EmbeddingBody>,
) -> Result<impl IntoResponse, ApiError> {
    let id = parse_drawer_id(&id)?;
    state
        .app
        .set_drawer_embedding(id, body.embedding, mode)
        .await?;
    Ok(Json(serde_json::json!({ "embedded": true })))
}

/// A drawer id from a path, with the shared invalid-input diagnostic.
fn parse_drawer_id(raw: &str) -> Result<DrawerId, crate::Error> {
    raw.parse().map_err(|_| {
        crate::Error::invalid_input("id", format!("`{raw}` is not a drawer id (a UUID)"))
    })
}

/// The body of `POST /api/drawers/{id}/mentions`.
#[derive(Debug, Deserialize)]
pub(super) struct MentionBody {
    /// The entity's name.
    name: String,
    /// The entity's kind (`person`, `project`, ...), free-form.
    kind: String,
}

/// `POST /api/drawers/{id}/mentions`: record that a drawer mentions an entity.
pub(super) async fn link_drawer_entity(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<MentionBody>,
) -> Result<impl IntoResponse, ApiError> {
    let id = parse_drawer_id(&id)?;
    let link = state
        .app
        .link_drawer_entity(id, &body.name, &body.kind, mode)
        .await?;
    Ok((created_status(link.created), Json(link)))
}
