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
    assert_eq!(parsed.profile_a.maxrate, "20M");

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

// ---------------------------------------------------------------------------
// Partial config files must still validate.
//
// `AppConfig` marks most sections `#[serde(default)]`, so a `config.toml` that
// omits one builds that struct through `Default`. Two of them derived
// `Default`, which ignores the per-field `#[serde(default = "...")]` entirely.
// A file with no `[server]` section therefore produced `web_port: 0` and an
// empty `bind_address`, and `validate()` rejected it; no `[encoding]` produced
// an empty `preset`, same outcome.
//
// That was not theoretical. `app::run_service` only auto-starts ingest when
// `validate()` succeeds, and it reports the failure to the UI log ring rather
// than to `tracing`. So the service came up, served its API, answered
// /api/health -- and silently never ingested anything, with nothing in the log
// to explain it. Found while verifying an unrelated change, which is the only
// reason it was found at all.
// ---------------------------------------------------------------------------

/// `validate()` requires the media roots to exist, so the fixtures are real.
struct Roots {
    dir: std::path::PathBuf,
    watch: String,
    target: String,
}

impl Drop for Roots {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn roots(name: &str) -> Roots {
    let dir = std::env::temp_dir().join(format!(
        "pt-cfgdocs-{}-{}-{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let watch = dir.join("watch");
    let target = dir.join("target");
    std::fs::create_dir_all(&watch).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    Roots {
        watch: watch.to_string_lossy().replace('\\', "/"),
        target: target.to_string_lossy().replace('\\', "/"),
        dir,
    }
}

fn minimal_config(r: &Roots) -> String {
    format!(
        "[paths]\nwatch_folder = \"{}\"\ntarget_folder = \"{}\"\n",
        r.watch, r.target
    )
}

#[test]
fn a_config_with_only_paths_is_valid() {
    let r = roots("minimal");
    let cfg: AppConfig = toml::from_str(&minimal_config(&r)).expect("must parse");
    if let Err(e) = cfg.validate() {
        panic!("a config with only [paths] must validate, or the service silently never auto-starts: {}", e);
    }
}

#[test]
fn omitting_any_single_section_still_validates() {
    let r = roots("sections");

    // The full documented config, with its placeholder paths pointed at real
    // directories, then one section dropped at a time.
    let full = readme_config_block();
    let parsed: toml::Value = toml::from_str(&full).expect("valid TOML");
    let mut table = parsed.as_table().expect("a table").clone();
    if let Some(paths) = table.get_mut("paths").and_then(|p| p.as_table_mut()) {
        paths.insert("watch_folder".into(), toml::Value::String(r.watch.clone()));
        paths.insert("target_folder".into(), toml::Value::String(r.target.clone()));
    }

    let sections: Vec<String> = table
        .iter()
        .filter(|(_, v)| v.is_table())
        // `[paths]` is genuinely required: the service cannot guess where the
        // media lives, and `validate()` says so explicitly.
        .filter(|(k, _)| k.as_str() != "paths")
        .map(|(k, _)| k.clone())
        .collect();

    for section in sections {
        let mut reduced = table.clone();
        reduced.remove(&section);

        let text = toml::to_string(&toml::Value::Table(reduced)).expect("serialisable");
        let cfg: AppConfig = toml::from_str(&text)
            .unwrap_or_else(|e| panic!("dropping [{}] made it unparseable: {}", section, e));

        if let Err(e) = cfg.validate() {
            panic!("dropping [{}] made the config invalid: {}", section, e);
        }
    }
}

#[test]
fn section_defaults_match_their_field_defaults() {
    // The specific trap: a struct deriving `Default` while its fields carry
    // `#[serde(default = "...")]`. The two must agree, or which one applies
    // depends on whether the section is present -- which is the bug.
    let r = roots("defaults");

    let present: AppConfig =
        toml::from_str(&format!("{}\n[server]\n[encoding]\n", minimal_config(&r)))
            .expect("parse");
    let absent: AppConfig = toml::from_str(&minimal_config(&r)).expect("parse");

    assert_eq!(
        present.server.web_port, absent.server.web_port,
        "an empty [server] and a missing [server] must produce the same port"
    );
    assert_eq!(
        present.server.bind_address, absent.server.bind_address,
        "...and the same bind address"
    );
    assert_eq!(
        present.encoding.preset, absent.encoding.preset,
        "an empty [encoding] and a missing [encoding] must produce the same preset"
    );
    assert_eq!(present.encoding.audio_bitrate, absent.encoding.audio_bitrate);

    // And they are the real values, not empty strings that happen to match.
    assert_eq!(absent.server.web_port, 4353);
    assert_eq!(absent.server.bind_address, "127.0.0.1");
    assert_eq!(absent.encoding.preset, "medium");
    assert_eq!(absent.encoding.audio_bitrate, "320k");
}

#[test]
fn the_built_in_defaults_validate() {
    // The other direction: what `AppConfig::default()` produces is what a fresh
    // install writes to disk, so it has to be runnable once paths are set.
    let r = roots("builtin");
    let mut cfg = AppConfig::default();
    cfg.paths.watch_folder = r.watch.clone();
    cfg.paths.target_folder = r.target.clone();

    if let Err(e) = cfg.validate() {
        panic!("the config a fresh install writes must be valid: {}", e);
    }
}
