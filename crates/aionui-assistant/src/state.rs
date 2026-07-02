//! Router state carrying the assistant service for axum handlers.

use std::sync::Arc;

// FORK-CUSTOM: deps needed to land skills/rules bundled inside a remote assistant package.
use aionui_db::ISkillRepository;
use aionui_extension::skill_service::SkillPaths;

use crate::service::AssistantService;

/// Shared state injected into `/api/assistants/*` handlers.
#[derive(Clone)]
pub struct AssistantRouterState {
    pub service: Arc<AssistantService>,
    /// FORK-CUSTOM: skill filesystem layout used when a remote assistant
    /// package bundles dependency skills under `skills/`. Written during
    /// `import_remote`; read-only otherwise.
    pub skill_paths: Arc<SkillPaths>,
    /// FORK-CUSTOM: skill metadata repository, paired with `skill_paths` to
    /// persist bundled skills via `import_skills_with_repo`.
    pub skill_repo: Arc<dyn ISkillRepository>,
    /// FORK-CUSTOM: shared registry that tracks skills bundled with a remote
    /// assistant package.  Shared with `SkillRouterState` so writes here are
    /// immediately visible to `GET /api/skills` without a disk round-trip.
    pub bundled_skill_registry: Arc<tokio::sync::Mutex<aionui_extension::AssistantSkillRegistry>>,
}

