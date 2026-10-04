//! The knowledge-graph read routes: entities, their relationships and the drawers that mention them.
//!
//! Read-only on purpose. Facts are written by checkpoints and by the extraction job, never over these routes, and
//! there is no MCP counterpart (docs/adr/024). Guarded by the authentication layer like every route but health.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use serde::Deserialize;

use crate::domain::EntityId;

use super::extract::ApiQuery;
use super::{ApiError, ApiState, ModeHeader};

fn parse_entity_id(raw: &str) -> Result<EntityId, crate::Error> {
    raw.parse().map_err(|_| {
        crate::Error::invalid_input("id", format!("`{raw}` is not an entity id (a UUID)"))
    })
}

#[derive(Debug, Deserialize)]
pub(super) struct ListEntitiesParams {
    /// Names containing this text, case-insensitively.
    name: Option<String>,
    kind: Option<String>,
    limit: Option<u32>,
}

/// `GET /api/entities`
pub(super) async fn list_entities(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    ApiQuery(params): ApiQuery<ListEntitiesParams>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        state
            .app
            .list_entities(
                params.name.as_deref(),
                params.kind.as_deref(),
                params.limit,
                mode,
            )
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
pub(super) struct RelationshipsParams {
    /// Also return superseded and retracted facts.
    #[serde(default)]
    include_expired: bool,
}

/// `GET /api/entities/{id}/relationships`
pub(super) async fn entity_relationships(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(id): Path<String>,
    ApiQuery(params): ApiQuery<RelationshipsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let id = parse_entity_id(&id)?;
    Ok(Json(
        state
            .app
            .entity_relationships(id, params.include_expired, mode)
            .await?,
    ))
}

/// `GET /api/entities/{id}/mentions`
pub(super) async fn entity_mentions(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id = parse_entity_id(&id)?;
    Ok(Json(state.app.entity_mentions(id, mode).await?))
}
