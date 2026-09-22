//! Ownership of the target media folder (T-3).
//!
//! Two registries sharing one media root is the accident that produced the
//! on-air failure this guard exists to prevent. A service was installed a
//! second time, out of a different data directory, pointed at the same
//! `target_folder`, and started publishing into it. Both registries then
//! described the same directory and neither described it correctly: PlayOut
//! held twelve rundown rows whose paths came from the retired registry, and the
//! first thing that noticed was the pre-flight check at TAKE.
//!
//! The mechanism is deliberately dull. A registry stamps the media root with
//! its own identity the first time it publishes there, and refuses to start
//! against a stamp belonging to somebody else. It is a marker file, not a lock:
//! it survives reboots, it is readable by an operator wondering which service
//! owns a folder, and removing it is the documented way to hand a folder over
//! on purpose.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MARKER_FILE_NAME: &str = ".playouttranscode-registry.json";

/// The stamp written into a media root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistryMarker {
    /// The owning registry's identity -- stable for the life of a database.
    pub registry_id: String,
    /// Where that registry lives, for the error message. Informational only:
    /// the comparison is on `registry_id`, because a data directory can be
    /// moved without changing whose registry it is.
    pub data_dir: String,
    pub claimed_at: String,
    pub service_version: String,
}

#[derive(Debug)]
pub enum ClaimError {
    /// The marker belongs to a different registry.
    Conflict {
        marker_path: PathBuf,
        ours: String,
        theirs: Box<RegistryMarker>,
    },
    /// The marker is there but unreadable, or the folder cannot be written.
    ///
    /// Refusing here too is deliberate. An unreadable marker means *something*
    /// claimed this folder and we cannot tell whether it was us.
    Io { path: PathBuf, message: String },
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::Conflict {
                marker_path,
                ours,
                theirs,
            } => write!(
                f,
                "The media folder is already owned by another PlayoutTranscode registry.\n  \
                 marker:     {}\n  owner:      registry {} at {}\n  claimed at: {}\n  \
                 this one:   registry {} at {}\n\
                 Two registries publishing into one media folder is how assets end up \
                 described by a registry that no longer owns them. Point this service at its \
                 own target folder, or -- if this folder really is being handed over -- \
                 delete the marker file and start again.",
                marker_path.display(),
                theirs.registry_id,
                theirs.data_dir,
                theirs.claimed_at,
                ours,
                crate::paths::data_dir().display(),
            ),
            ClaimError::Io { path, message } => write!(
                f,
                "Cannot establish ownership of the media folder '{}': {}",
                path.display(),
                message
            ),
        }
    }
}

impl std::error::Error for ClaimError {}

/// Claim `target_root` for the registry identified by `registry_id`.
///
/// Writes the marker if the folder is unclaimed, accepts it if we already own
/// it, and refuses otherwise.
pub fn claim(target_root: &Path, registry_id: &str) -> Result<RegistryMarker, ClaimError> {
    let marker_path = target_root.join(MARKER_FILE_NAME);

    if let Err(e) = std::fs::create_dir_all(target_root) {
        return Err(ClaimError::Io {
            path: target_root.to_path_buf(),
            message: e.to_string(),
        });
    }

    match std::fs::read_to_string(&marker_path) {
        Ok(raw) => {
            let existing: RegistryMarker =
                serde_json::from_str(&raw).map_err(|e| ClaimError::Io {
                    path: marker_path.clone(),
                    message: format!("the ownership marker is not readable: {}", e),
                })?;
            if existing.registry_id == registry_id {
                Ok(existing)
            } else {
                Err(ClaimError::Conflict {
                    marker_path,
                    ours: registry_id.to_string(),
                    theirs: Box::new(existing),
                })
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let marker = RegistryMarker {
                registry_id: registry_id.to_string(),
                data_dir: crate::paths::data_dir().to_string_lossy().into_owned(),
                claimed_at: chrono::Utc::now().to_rfc3339(),
                service_version: env!("CARGO_PKG_VERSION").to_string(),
            };
            write_marker(&marker_path, &marker)?;
            tracing::info!(
                "Claimed media folder {} for registry {}",
                target_root.display(),
                registry_id
            );
            Ok(marker)
        }
        Err(e) => Err(ClaimError::Io {
            path: marker_path,
            message: e.to_string(),
        }),
    }
}

fn write_marker(marker_path: &Path, marker: &RegistryMarker) -> Result<(), ClaimError> {
    let json = serde_json::to_string_pretty(marker).map_err(|e| ClaimError::Io {
        path: marker_path.to_path_buf(),
        message: e.to_string(),
    })?;
    let tmp = marker_path.with_extension("tmp");
    std::fs::write(&tmp, &json).map_err(|e| ClaimError::Io {
        path: tmp.clone(),
        message: e.to_string(),
    })?;
    std::fs::rename(&tmp, marker_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        ClaimError::Io {
            path: marker_path.to_path_buf(),
            message: e.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("pt_media_root_{}_{}", tag, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_unclaimed_folder_is_claimed_and_the_marker_persists() {
        let root = temp_root("fresh");
        let marker = claim(&root, "registry-a").expect("a fresh folder must be claimable");
        assert_eq!(marker.registry_id, "registry-a");
        assert!(root.join(MARKER_FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_same_registry_reclaims_its_own_folder_on_every_start() {
        let root = temp_root("same");
        let first = claim(&root, "registry-a").unwrap();
        let second = claim(&root, "registry-a").unwrap();
        assert_eq!(first, second, "reclaiming must not rewrite the marker");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The two-registry accident, reproduced.
    #[test]
    fn a_second_registry_is_refused() {
        let root = temp_root("conflict");
        claim(&root, "registry-a").unwrap();
        let err = claim(&root, "registry-b").expect_err("a foreign marker must refuse the claim");
        match &err {
            ClaimError::Conflict { theirs, ours, .. } => {
                assert_eq!(theirs.registry_id, "registry-a");
                assert_eq!(ours, "registry-b");
            }
            other => panic!("expected a conflict, got {:?}", other),
        }
        // And the message names the file the operator has to deal with.
        assert!(err.to_string().contains(MARKER_FILE_NAME));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_corrupt_marker_refuses_rather_than_overwriting_it() {
        let root = temp_root("corrupt");
        std::fs::write(root.join(MARKER_FILE_NAME), "{not json").unwrap();
        let err = claim(&root, "registry-a").expect_err("an unreadable marker must refuse");
        assert!(matches!(err, ClaimError::Io { .. }));
        let _ = std::fs::remove_dir_all(&root);
    }
}
