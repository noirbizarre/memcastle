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
