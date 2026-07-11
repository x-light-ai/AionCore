// FORK-CUSTOM: remote assistant package import helpers.
//
// Implements "assistant + dependency skills + rule packaged as one zip" on top
// of the App-level `/api/assistants/import-remote` route. Upstream assistant
// routes and router state remain unchanged.
//
// Package layout (one assistant per zip):
//   assistant-market.zip
//   ├── assistants.json   # { "assistants": [ single CreateAssistantRequest ] }
//   ├── RULE.md           # system prompt (default locale), fixed name
//   ├── RULE.<locale>.md  # optional, per-locale
//   └── skills/<name>/SKILL.md   # optional dependency skills

// This is import-remote route-handler glue, so it maps to `ApiError` at the
// HTTP boundary.
#![allow(clippy::disallowed_types)]

use std::path::Path;

use aionui_api_types::ImportAssistantsRequest;
use aionui_common::ApiError;
use tracing::warn;

use aionui_assistant::service::generate_user_id;

use super::assistant_routes::XaiworkAssistantState;
use super::skill_metadata::{persist_assistant_bundle_metadata, snapshot_installed_skill_metadata};

/// Ensure the single packaged assistant carries a stable id before import, so
/// the bundled `RULE.md` can be written to the same id afterwards. Returns the
/// id to use for rule landing (the first assistant in the manifest), or `None`
/// when the manifest is empty.
pub(crate) fn ensure_packaged_assistant_id(req: &mut ImportAssistantsRequest) -> Option<String> {
    let entry = req.assistants.first_mut()?;
    if entry.id.as_deref().map(str::trim).is_none_or(str::is_empty) {
        entry.id = Some(generate_user_id());
    }
    entry.id.clone()
}

/// Land dependency skills bundled under `skills/` using the standard skill
/// import pipeline. No-op when the package carries no `skills/` directory
/// (pure assistant package — backward compatible).
///
/// Returns `Err` only when the overall skill import fails; per-skill failures
/// are logged at `warn` and do not block the assistant import.
pub(crate) async fn import_bundled_skills(
    state: &XaiworkAssistantState,
    extract_dir: &Path,
    assistant_id: Option<&str>,
) -> Result<(), ApiError> {
    let skills_dir = extract_dir.join("skills");
    if !skills_dir.is_dir() {
        return Ok(());
    }

    let previous = snapshot_installed_skill_metadata(&state.skill_paths).await;
    let outcome = aionui_extension::skill_service::import_skills_with_repo(
        state.skill_paths.as_ref(),
        state.skill_repo.as_ref(),
        &skills_dir,
    )
    .await?;

    if !outcome.failed.is_empty() {
        warn!(
            imported_count = outcome.imported.len(),
            failed_count = outcome.failed.len(),
            failures = ?outcome.failed,
            "bundled assistant skill import completed with failures"
        );
    }

    if let Some(assistant_id) = assistant_id
        && !outcome.imported.is_empty()
    {
        persist_assistant_bundle_metadata(&state.skill_paths, &outcome.imported, assistant_id, &previous).await?;
    }

    Ok(())
}

/// Write the bundled `RULE.md` / `RULE.<locale>.md` (system prompt) for the
/// imported assistant. Best-effort: a missing rule file or a write failure is
/// logged at `warn` and never fails the already-completed assistant import.
pub(crate) async fn apply_bundled_rule(state: &XaiworkAssistantState, extract_dir: &Path, assistant_id: &str) {
    let read_dir = match std::fs::read_dir(extract_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in read_dir.flatten() {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(locale) = parse_rule_locale(file_name) else {
            continue;
        };

        let content = match std::fs::read_to_string(entry.path()) {
            Ok(content) => content,
            Err(error) => {
                warn!(assistant_id, file = file_name, error = %error, "read bundled assistant rule failed");
                continue;
            }
        };

        if let Err(error) = state
            .service
            .write_rule(assistant_id, locale.as_deref(), &content)
            .await
        {
            warn!(assistant_id, locale = ?locale, error = %error, "write bundled assistant rule failed");
        }
    }
}

/// Parse a bundled rule filename into its locale.
///
/// - `RULE.md`       -> `Some(None)`            (default locale)
/// - `RULE.zh-CN.md` -> `Some(Some("zh-CN"))`
/// - anything else   -> `None`
fn parse_rule_locale(file_name: &str) -> Option<Option<String>> {
    if file_name == "RULE.md" {
        return Some(None);
    }
    let rest = file_name.strip_prefix("RULE.")?;
    let locale = rest.strip_suffix(".md")?;
    if locale.is_empty() {
        None
    } else {
        Some(Some(locale.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aionui_api_types::CreateAssistantRequest;

    fn req_with_id(id: Option<&str>) -> ImportAssistantsRequest {
        ImportAssistantsRequest {
            assistants: vec![CreateAssistantRequest {
                id: id.map(str::to_string),
                name: "Demo".into(),
                description: None,
                avatar: None,
                agent_id: None,
                enabled_skills: None,
                custom_skill_names: None,
                disabled_builtin_skills: None,
                prompts: None,
                models: None,
                name_i18n: None,
                description_i18n: None,
                prompts_i18n: None,
                recommended_prompts: None,
                recommended_prompts_i18n: None,
                defaults: None,
            }],
        }
    }

    #[test]
    fn ensure_id_generates_when_missing() {
        let mut req = req_with_id(None);
        let id = ensure_packaged_assistant_id(&mut req);
        assert!(id.is_some());
        assert_eq!(req.assistants[0].id, id);
    }

    #[test]
    fn ensure_id_generates_when_blank() {
        let mut req = req_with_id(Some("   "));
        let id = ensure_packaged_assistant_id(&mut req);
        assert!(id.as_deref().is_some_and(|v| !v.trim().is_empty()));
        assert_eq!(req.assistants[0].id, id);
    }

    #[test]
    fn ensure_id_keeps_explicit() {
        let mut req = req_with_id(Some("my-assistant"));
        let id = ensure_packaged_assistant_id(&mut req);
        assert_eq!(id.as_deref(), Some("my-assistant"));
    }

    #[test]
    fn parse_rule_locale_variants() {
        assert_eq!(parse_rule_locale("RULE.md"), Some(None));
        assert_eq!(parse_rule_locale("RULE.zh-CN.md"), Some(Some("zh-CN".to_string())));
        assert_eq!(parse_rule_locale("RULE.en-US.md"), Some(Some("en-US".to_string())));
        assert_eq!(parse_rule_locale("SKILL.md"), None);
        assert_eq!(parse_rule_locale("assistants.json"), None);
        assert_eq!(parse_rule_locale("RULE..md"), None);
    }
}
