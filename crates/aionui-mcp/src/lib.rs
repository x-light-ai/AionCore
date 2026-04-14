pub mod adapter;
pub mod error;
pub mod routes;
pub mod service;
pub mod types;

pub use adapter::{DetectedServer, McpAgentAdapter};
pub use error::McpError;
pub use routes::{McpRouterState, mcp_routes};
pub use service::McpConfigService;
pub use types::{McpServer, McpServerTransport, McpTool};
