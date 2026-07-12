// FORK-CUSTOM: isolate XAIWork's empty builtin Assistant policy from upstream loaders.

use std::sync::Arc;

use aionui_assistant::BuiltinAssistantRegistry;

/// XAIWork installs assistants from its market and never seeds upstream presets.
pub(crate) fn builtin_assistant_registry() -> Arc<BuiltinAssistantRegistry> {
    Arc::new(BuiltinAssistantRegistry::empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xaiwork_builtin_assistant_registry_is_empty() {
        assert!(builtin_assistant_registry().is_empty());
    }
}
