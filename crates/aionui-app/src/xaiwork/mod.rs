// FORK-CUSTOM: XAIWork integration boundary composed outside upstream domains.
//! XAIWork integration boundary.

use std::sync::Arc;

use aionui_auth::{AuthState, auth_middleware};
use axum::Router;
use axum::middleware::from_fn_with_state;

use crate::router::ModuleStates;
use crate::services::AppServices;

pub(crate) mod agent_config;
pub(crate) mod agent_remote;
pub(crate) mod agent_routes;
pub(crate) mod assistant_import;
pub(crate) mod assistant_policy;
pub(crate) mod assistant_routes;
pub(crate) mod auth;
pub(crate) mod config;
pub(crate) mod skill_metadata;
pub(crate) mod skill_routes;

use agent_routes::{XaiworkAgentState, xaiwork_agent_routes};
use assistant_routes::{XaiworkAssistantState, xaiwork_assistant_routes};
use auth::{XaiworkAuthState, xaiwork_auth_routes};
use config::resolve_xaiwork_base_url;
use skill_routes::{XaiworkSkillState, xaiwork_skill_routes};

/// Assemble all fork routes behind one App-level integration point.
pub(crate) fn xaiwork_routes(services: &AppServices, states: &ModuleStates, auth_state: AuthState) -> Router {
    let skill_paths = Arc::new(states.skill.skill_paths.clone());
    let skill_repo = states.skill.skill_repo.clone();

    let authenticated = Router::new()
        .merge(xaiwork_agent_routes(XaiworkAgentState {
            registry: services.agent_registry.clone(),
        }))
        .merge(xaiwork_assistant_routes(XaiworkAssistantState {
            service: states.assistant.service.clone(),
            skill_paths: skill_paths.clone(),
            skill_repo: skill_repo.clone(),
        }))
        .merge(xaiwork_skill_routes(XaiworkSkillState {
            skill_paths,
            skill_repo,
        }))
        .route_layer(from_fn_with_state(auth_state, auth_middleware));

    Router::new()
        .merge(xaiwork_auth_routes(XaiworkAuthState {
            jwt_service: services.jwt_service.clone(),
            user_repo: services.user_repo.clone(),
            cookie_config: services.cookie_config.clone(),
            base_url: resolve_xaiwork_base_url(&services.data_dir),
        }))
        .merge(authenticated)
}
