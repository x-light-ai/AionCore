pub mod agent;
pub mod availability;
pub mod custom;
pub mod provider_health;
pub mod remote;
// FORK-CUSTOM: unified model config service (placed at end of the mod list to
// avoid colliding with upstream module additions).
pub mod xaiwork_builtin_config;
// FORK-CUSTOM: XAIWork server-to-server config broker (fetches credentials
// from XAIWork OpenApi so the AionUi renderer never sees api_key).
pub mod xaiwork_config;

pub use agent::AgentService;
pub use availability::AgentAvailabilityFeedbackPort;
pub use remote::RemoteAgentService;
