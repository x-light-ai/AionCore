// FORK-CUSTOM: regression guard for the "ship no builtin skills" fork feature.
//
// This fork points the `include_dir!` macro in
// `aionui-extension/src/skill_service.rs` at `assets/builtin-skills-empty`
// instead of the upstream `assets/builtin-skills`, so the binary embeds no
// builtin skills (all skills install on demand from the Skill Market).
//
// That change is a single-line edit to an upstream file, which makes it easy
// to silently lose during an upstream rebase/merge — the code still compiles
// either way. This test turns that silent regression into a hard `cargo test`
// failure: if a merge restores the full corpus, the assertions below fail.
//
// See fork-features-aioncore.md "功能 5: 按需安装 Builtin Skills".

use aionui_extension::builtin_skills_corpus;

/// The embedded corpus must contain zero skill directories. Each builtin
/// skill ships as its own subdirectory holding a `SKILL.md`, so a non-empty
/// `dirs()` means the upstream corpus was re-embedded.
#[test]
fn embedded_builtin_skills_corpus_is_empty() {
    let corpus = builtin_skills_corpus();

    let skill_dirs: Vec<_> = corpus.dirs().map(|d| d.path().to_path_buf()).collect();
    assert!(
        skill_dirs.is_empty(),
        "FORK-CUSTOM regression: builtin-skills corpus must be empty, but found {} skill dir(s): {:?}. \
         The `include_dir!` in skill_service.rs was likely reverted to `assets/builtin-skills` by an \
         upstream merge — point it back at `assets/builtin-skills-empty`.",
        skill_dirs.len(),
        skill_dirs,
    );
}

/// Source-text guard: assert that `skill_service.rs` literally contains the
/// `include_dir!` call pointing at `builtin-skills-empty`. This complements
/// the semantic check above — it catches a revert even if the corpus happened
/// to look empty for some other reason, and pins the exact macro argument an
/// upstream merge is most likely to clobber. `include_str!` embeds the source
/// at compile time, so there is no runtime file IO or cwd dependency.
#[test]
fn skill_service_source_points_at_empty_corpus() {
    const SKILL_SERVICE_SRC: &str = include_str!("../src/skill_service.rs");
    const EXPECTED: &str = r#"include_dir!("$CARGO_MANIFEST_DIR/../aionui-app/assets/builtin-skills-empty")"#;

    assert!(
        SKILL_SERVICE_SRC.contains(EXPECTED),
        "FORK-CUSTOM regression: skill_service.rs must contain `{EXPECTED}`. \
         The `include_dir!` line was likely reverted to `assets/builtin-skills` by an \
         upstream merge — point it back at `assets/builtin-skills-empty`.",
    );
}
