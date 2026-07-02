// FORK-CUSTOM: XAIWork-specific runtime guards layered on top of upstream
// agent dispatch. Lives outside `cc_switch/` so the upstream cc-switch
// module stays untouched and future upstream changes to cc-switch don't
// collide with fork-only logic.

use aionui_common::EnvVar;

/// Returns true when XAIWork has populated the Claude relay env keys via
/// `set_builtin_agent_config`. When this is true, callers should treat the
/// env as XAIWork-managed and skip any other provider switcher (e.g.
/// upstream's cc-switch) that would otherwise override `ANTHROPIC_BASE_URL`.
///
/// The marker key is `ANTHROPIC_BASE_URL`: `applyXaiworkModelConfig` always
/// writes it together with `ANTHROPIC_AUTH_TOKEN`/`ANTHROPIC_MODEL`, so
/// presence is a reliable signal that the relay config has been applied.
pub fn xaiwork_env_managed(env: &[EnvVar]) -> bool {
    env.iter().any(|e| e.name == "ANTHROPIC_BASE_URL")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_managed_when_marker_key_present() {
        let env = vec![EnvVar {
            name: "ANTHROPIC_BASE_URL".into(),
            value: "https://relay.example.com".into(),
        }];
        assert!(xaiwork_env_managed(&env));
    }

    #[test]
    fn returns_false_on_empty_env() {
        assert!(!xaiwork_env_managed(&[]));
    }

    #[test]
    fn returns_false_when_other_keys_only() {
        let env = vec![
            EnvVar {
                name: "ANTHROPIC_AUTH_TOKEN".into(),
                value: "x".into(),
            },
            EnvVar {
                name: "ANTHROPIC_MODEL".into(),
                value: "claude".into(),
            },
        ];
        assert!(!xaiwork_env_managed(&env));
    }
}
