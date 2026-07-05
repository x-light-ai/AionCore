// FORK-CUSTOM: locale fallback for assistant rule reads.
//
// This file is XAIWork fork-only and does not exist upstream; keeping the
// fallback policy here minimizes rebase conflict risk in the upstream service.

use std::path::Path;

/// Read a locale-specific rule file, falling back to the default rule file when
/// the locale file is absent or unreadable.
///
/// Empty locale files are treated as explicit content and do not fall back.
pub fn read_user_rule_with_default_fallback(localized_path: &Path, default_path: &Path) -> String {
    match std::fs::read_to_string(localized_path) {
        Ok(content) => content,
        Err(_) if localized_path != default_path => std::fs::read_to_string(default_path).unwrap_or_default(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_default_when_locale_file_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let default_path = temp.path().join("assistant.md");
        let localized_path = temp.path().join("assistant.zh-CN.md");
        std::fs::write(&default_path, "default rule").unwrap();

        let content = read_user_rule_with_default_fallback(&localized_path, &default_path);

        assert_eq!(content, "default rule");
    }

    #[test]
    fn locale_file_wins_when_present() {
        let temp = tempfile::tempdir().unwrap();
        let default_path = temp.path().join("assistant.md");
        let localized_path = temp.path().join("assistant.zh-CN.md");
        std::fs::write(&default_path, "default rule").unwrap();
        std::fs::write(&localized_path, "localized rule").unwrap();

        let content = read_user_rule_with_default_fallback(&localized_path, &default_path);

        assert_eq!(content, "localized rule");
    }

    #[test]
    fn empty_locale_file_does_not_fall_back() {
        let temp = tempfile::tempdir().unwrap();
        let default_path = temp.path().join("assistant.md");
        let localized_path = temp.path().join("assistant.zh-CN.md");
        std::fs::write(&default_path, "default rule").unwrap();
        std::fs::write(&localized_path, "").unwrap();

        let content = read_user_rule_with_default_fallback(&localized_path, &default_path);

        assert!(content.is_empty());
    }
}
