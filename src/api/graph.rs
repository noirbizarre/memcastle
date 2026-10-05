//! The knowledge-graph routes: entities, their relationships and the drawers that mention them.
//!
//! Read-only apart from one write, an alias that settles an ambiguous name by hand (docs/adr/025). Facts are
//! written by checkpoints and by the extraction job, never over these routes, and there is no MCP counterpart
//! (docs/adr/024). Guarded by the authentication layer like every route but health.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Json};
use serde::Deserialize;

use crate::domain::EntityId;

use super::extract::{ApiJson, ApiQuery};
use super::{ApiError, ApiState, ModeHeader};

fn parse_entity_id(raw: &str) -> Result<EntityId, crate::Error> {
    crate::Error::parse_entity_id("id", raw)
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
pub(super) struct GraphParams {
    /// The entity to draw the neighbourhood of. Without it, the first entities by name.
    entity: Option<String>,
    /// Hops to follow from `entity`, 1 to 3. Default 1.
    depth: Option<u32>,
    /// The most entities in the answer (default 50, at most 200).
    limit: Option<u32>,
}

/// `GET /api/graph`: entities and the facts between them in one answer, for drawing.
pub(super) async fn graph(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    ApiQuery(params): ApiQuery<GraphParams>,
) -> Result<impl IntoResponse, ApiError> {
    let entity = params.entity.as_deref().map(parse_entity_id).transpose()?;
    Ok(Json(
        state
            .app
            .graph(entity, params.depth, params.limit, mode)
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

/// `GET /api/entities/{id}/candidates`: the entities this one resembles without having been equated with, or that
/// resemble it. Nothing here is merged.
pub(super) async fn entity_candidates(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id = parse_entity_id(&id)?;
    Ok(Json(state.app.entity_candidates(id, mode).await?))
}

#[derive(Debug, Deserialize)]
pub(super) struct AliasBody {
    /// Another spelling of the entity.
    alias: String,
}

/// `POST /api/entities/{id}/aliases`: record another spelling of an entity, so later sightings converge on it.
pub(super) async fn add_entity_alias(
    State(state): State<ApiState>,
    ModeHeader(mode): ModeHeader,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<AliasBody>,
) -> Result<impl IntoResponse, ApiError> {
    let id = parse_entity_id(&id)?;
    Ok(Json(
        state.app.add_entity_alias(id, &body.alias, mode).await?,
    ))
}
