//! The README's configuration block has to be true (T3-4, F-25).
//!
//! It described `[transcode]`, `[audio]` and `[cleanup]` sections, and a
//! `paths.database_path` key, none of which the service has ever read. An
//! operator who copied it got a file that parsed, started cleanly, and applied
//! none of what they had written — serde ignores unknown fields silently.
//!
//! These tests make the documentation a build artifact rather than prose.

use playout_transcode::config::AppConfig;

/// The first ```toml fenced block in README.md.
fn readme_config_block() -> String {
    let readme = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"),
    )
    .expect("README.md must be readable");

    let fence = "`".repeat(3);
    let open = format!("{}toml", fence);
    let start = readme
        .find(&open)
        .expect("README must carry a ```toml configuration block")
        + open.len();
    let end = start
        + readme[start..]
            .find(&fence)
            .expect("the ```toml block must be closed");
    readme[start..end].to_string()
}

#[test]
fn the_documented_config_parses_into_the_real_schema() {
    let block = readme_config_block();
    let parsed: Result<AppConfig, _> = toml::from_str(&block);
    assert!(
        parsed.is_ok(),
        "the README's config block does not deserialise into AppConfig: {}",
        parsed.unwrap_err()
    );
}

#[test]
fn the_documented_config_names_no_key_the_service_does_not_read() {
    // The real assertion. Parsing proves nothing on its own -- serde accepts a
    // file full of invented sections without complaint, which is exactly how
    // the old block survived so long.
    let block = readme_config_block();
    let raw: toml::Value = toml::from_str(&block).expect("valid TOML");

    let unknown = AppConfig::unknown_keys(&raw);
    assert!(
        unknown.is_empty(),
        "README documents {} key(s) the service never reads: {:?}",
        unknown.len(),
        unknown
    );
}

#[test]
fn the_documented_values_survive_a_round_trip() {
    // A block that parses but loses its values on re-serialisation would still
    // mislead: it would mean the schema silently drops what is written.
    let block = readme_config_block();
    let parsed: AppConfig = toml::from_str(&block).expect("valid config");

    assert_eq!(parsed.server.web_port, 4353);
    assert_eq!(parsed.ingestion.max_concurrency, 2);
    assert_eq!(parsed.logging.retain_days, 14);
    assert_eq!(parsed.profile_a.maxrate, "15M");

    // The optional policy sections have to actually land, not be swallowed.
    let validation = parsed
        .validation_policy
        .as_ref()
        .expect("[validation_policy] must deserialise into the Option");
    assert_eq!(validation.max_duration_delta_ms, 80);
    assert!(parsed.audio_policy.is_some(), "[audio_policy]");
    assert!(parsed.toolchain_policy.is_some(), "[toolchain_policy]");
    assert!(parsed.retry_policy_v2.is_some(), "[retry_policy_v2]");
}

#[test]
fn a_fresh_config_file_names_no_unknown_key_either() {
    // The other direction: what the service writes on a fresh install must
    // itself be fully covered by `known_keys`, or the warning fires on a file
    // the service produced.
    let defaults = AppConfig::default();
    let written = toml::to_string_pretty(&defaults).expect("serialisable");
    let raw: toml::Value = toml::from_str(&written).expect("valid TOML");

    let unknown = AppConfig::unknown_keys(&raw);
    assert!(
        unknown.is_empty(),
        "the service writes key(s) its own validator rejects: {:?}",
        unknown
    );
}

#[test]
fn the_sections_the_readme_used_to_invent_are_reported_as_unknown() {
    // A regression guard on the detector. If `unknown_keys` ever stopped
    // catching these, the README test above would pass vacuously.
    let bogus: toml::Value = toml::from_str(
        r#"
[paths]
watch_folder = "D:/in"
target_folder = "D:/out"
database_path = "D:/logs/media_assets.db"

[transcode]
default_profile = "ProfileA"
max_concurrency = 2

[audio]
mode = "ebu_r128"

[cleanup]
auto_purge_days = 30
"#,
    )
    .unwrap();

    let unknown = AppConfig::unknown_keys(&bogus);
    assert!(unknown.contains(&"transcode".to_string()), "{:?}", unknown);
    assert!(unknown.contains(&"audio".to_string()), "{:?}", unknown);
    assert!(unknown.contains(&"cleanup".to_string()), "{:?}", unknown);
    assert!(
        unknown.contains(&"paths.database_path".to_string()),
        "a bad key inside a real section must be caught too: {:?}",
        unknown
    );
}

#[test]
fn a_misplaced_key_inside_a_real_section_is_caught() {
    // The likeliest real mistake: right key, wrong section.
    let v: toml::Value = toml::from_str(
        r#"
max_concurrency = 4

[paths]
watch_folder = "D:/in"
target_folder = "D:/out"

[ingestion]
max_concurrancy = 4
"#,
    )
    .unwrap();

    let unknown = AppConfig::unknown_keys(&v);
    assert!(
        unknown.contains(&"max_concurrency".to_string()),
        "a top-level scalar that belongs in a section: {:?}",
        unknown
    );
    assert!(
        unknown.contains(&"ingestion.max_concurrancy".to_string()),
        "a typo inside a real section: {:?}",
        unknown
    );
}
