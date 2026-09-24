//! Source-file identity (T2-6).
//!
//! Two functions, used as a pair:
//!
//! * [`compute_sampled_fingerprint`] hashes the file size plus the first,
//!   middle and last 64 KiB. It is cheap — three seeks, constant cost — and it
//!   is the indexed column, so it is the prefilter.
//! * [`compute_full_sha256`] hashes every byte. It is the confirmation.
//!
//! The sampled hash on its own was the dedup decision, and that is F-15: two
//! distinct programmes cut from the same master — same size, same leader, same
//! tail, different content in the middle-minus-32-KiB — collide, and the second
//! one is silently dropped as a duplicate. In a broadcast library that is not a
//! hypothetical: promos and versioned cuts are produced exactly that way.
//!
//! The pair keeps the common path cheap. The full hash is computed only after
//! the sampled one has already matched, which for a library of distinct media
//! is approximately never.
//!
//! The old name was `compute_fnv1a64`, which was wrong on both counts: it is
//! SHA-256, truncated to 64 bits, and it was never FNV.

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const CHUNK_SIZE: usize = 64 * 1024;
/// 1024 chunks of 64 KiB: the full hash looks for a stop every 64 MiB.
const INTERRUPT_CHECK_CHUNKS: u64 = 1024;

/// A cheap, indexed prefilter: size plus three 64 KiB samples.
///
/// A match means "these two files are worth comparing properly", never "these
/// two files are the same". Callers must confirm with [`compute_full_sha256`].
pub fn compute_sampled_fingerprint(path: &Path) -> Result<i64, String> {
    let mut file = File::open(path).map_err(|e| format!("Fingerprint open failed: {}", e))?;

    let metadata = file
        .metadata()
        .map_err(|e| format!("Fingerprint metadata failed: {}", e))?;
    let file_size = metadata.len();

    let mut hasher = Sha256::new();
    hasher.update(&file_size.to_le_bytes());

    if file_size <= (CHUNK_SIZE as u64 * 3) {
        let mut buf = vec![0u8; file_size as usize];
        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("Fingerprint seek failed: {}", e))?;
        file.read_exact(&mut buf)
            .map_err(|e| format!("Fingerprint read failed: {}", e))?;
        hasher.update(&buf);
    } else {
        let mut head = vec![0u8; CHUNK_SIZE];
        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("Fingerprint seek failed: {}", e))?;
        file.read_exact(&mut head)
            .map_err(|e| format!("Fingerprint read failed: {}", e))?;
        hasher.update(&head);

        let mid_offset = file_size / 2 - (CHUNK_SIZE as u64 / 2);
        let mut mid = vec![0u8; CHUNK_SIZE];
        file.seek(SeekFrom::Start(mid_offset))
            .map_err(|e| format!("Fingerprint seek failed: {}", e))?;
        file.read_exact(&mut mid)
            .map_err(|e| format!("Fingerprint read failed: {}", e))?;
        hasher.update(&mid);

        let mut tail = vec![0u8; CHUNK_SIZE];
        file.seek(SeekFrom::End(-(CHUNK_SIZE as i64)))
            .map_err(|e| format!("Fingerprint seek failed: {}", e))?;
        file.read_exact(&mut tail)
            .map_err(|e| format!("Fingerprint read failed: {}", e))?;
        hasher.update(&tail);
    }

    let hash = hasher.finalize();
    let truncated = u64::from_be_bytes(hash[0..8].try_into().unwrap());
    Ok(truncated as i64)
}

/// SHA-256 of the entire file, lowercase hex.
///
/// Streamed in 64 KiB blocks, so memory is constant regardless of file size —
/// these are broadcast masters and can be tens of gigabytes.
pub fn compute_full_sha256(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("Full hash open failed: {}", e))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut chunks: u64 = 0;
    loop {
        // Every 64 MiB (handoff #6): a stop or a cancel used to wait out the
        // whole file, 40 GB on a share. The check is a queue lookup, noise
        // next to 64 MiB of I/O.
        if chunks.is_multiple_of(INTERRUPT_CHECK_CHUNKS) && crate::child::interrupted() {
            return Err(format!("Full hash {}", crate::child::INTERRUPTED));
        }
        chunks += 1;
        let read = file
            .read(&mut buffer)
            .map_err(|e| format!("Full hash read failed: {}", e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn an_interrupted_full_hash_stops_instead_of_reading_to_the_end() {
        let path = std::env::temp_dir().join(format!("fp_interrupt_{}.bin", std::process::id()));
        std::fs::write(&path, vec![0u8; 1024]).unwrap();
        {
            let _scope = crate::child::interrupt_scope(std::sync::Arc::new(|| true));
            let err = compute_full_sha256(&path).unwrap_err();
            assert!(err.contains(crate::child::INTERRUPTED), "{err}");
        }
        assert!(compute_full_sha256(&path).is_ok());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_fingerprint_stability() {
        let dir = std::env::temp_dir();
        let path1 = dir.join("fp_test_1.bin");
        let path2 = dir.join("fp_test_2.bin");

        {
            let mut f = File::create(&path1).unwrap();
            f.write_all(&vec![0xABu8; 200_000]).unwrap();
        }
        {
            let mut f = File::create(&path2).unwrap();
            f.write_all(&vec![0xABu8; 200_000]).unwrap();
        }

        let fp1 = compute_sampled_fingerprint(&path1).unwrap();
        let fp2 = compute_sampled_fingerprint(&path2).unwrap();
        assert_eq!(
            fp1, fp2,
            "Identical content should produce identical fingerprints"
        );

        let _ = std::fs::remove_file(&path1);
        let _ = std::fs::remove_file(&path2);
    }

    #[test]
    fn test_fingerprint_differs_on_content_change() {
        let dir = std::env::temp_dir();
        let path_a = dir.join("fp_diff_a.bin");
        let path_b = dir.join("fp_diff_b.bin");

        {
            let mut f = File::create(&path_a).unwrap();
            f.write_all(&vec![0xAAu8; 300_000]).unwrap();
        }
        {
            let mut f = File::create(&path_b).unwrap();
            f.write_all(&vec![0xBBu8; 300_000]).unwrap();
        }

        let fp_a = compute_sampled_fingerprint(&path_a).unwrap();
        let fp_b = compute_sampled_fingerprint(&path_b).unwrap();
        assert_ne!(
            fp_a, fp_b,
            "Different content should produce different fingerprints"
        );

        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }
    /// The collision F-15 is about, built deliberately.
    ///
    /// Same size, byte-identical first / middle / last 64 KiB, different
    /// everywhere else — which is what a re-cut of the same master looks like.
    /// The sampled fingerprint cannot tell them apart and must not be asked to.
    #[test]
    fn two_recuts_of_one_master_collide_on_the_sampled_hash_but_not_the_full_one() {
        let dir = std::env::temp_dir();
        let a = dir.join(format!("fp_collide_a_{}.bin", std::process::id()));
        let b = dir.join(format!("fp_collide_b_{}.bin", std::process::id()));

        // 512 KiB: comfortably past the 3 x 64 KiB "small file" path, so the
        // sampled hash really does read head / middle / tail and skip the rest.
        let size = 512 * 1024;
        let mut content_a = vec![0x11u8; size];
        let mut content_b = vec![0x11u8; size];

        // Differ only in a region no sample touches. The middle sample covers
        // size/2 - 32 KiB .. size/2 + 32 KiB, so 100 KiB in is safely outside
        // the head (0..64 KiB) and outside the middle window.
        for i in 100_000..110_000 {
            content_a[i] = 0xAA;
            content_b[i] = 0xBB;
        }

        File::create(&a).unwrap().write_all(&content_a).unwrap();
        File::create(&b).unwrap().write_all(&content_b).unwrap();

        let sampled_a = compute_sampled_fingerprint(&a).unwrap();
        let sampled_b = compute_sampled_fingerprint(&b).unwrap();
        assert_eq!(
            sampled_a, sampled_b,
            "the sampled hash is expected to collide here -- that is the whole \
             reason the full hash exists"
        );

        let full_a = compute_full_sha256(&a).unwrap();
        let full_b = compute_full_sha256(&b).unwrap();
        assert_ne!(
            full_a, full_b,
            "the full hash must separate them, or the second programme is \
             silently dropped as a duplicate (F-15)"
        );

        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }

    #[test]
    fn the_full_hash_is_stable_and_lowercase_hex() {
        let dir = std::env::temp_dir();
        let p = dir.join(format!("fp_sha_{}.bin", std::process::id()));
        File::create(&p).unwrap().write_all(b"playout").unwrap();

        let h = compute_full_sha256(&p).unwrap();
        assert_eq!(h.len(), 64, "sha256 is 32 bytes of hex");
        assert!(h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(h, compute_full_sha256(&p).unwrap(), "must be deterministic");

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn hashing_a_missing_file_is_an_error_not_a_panic() {
        let missing = std::env::temp_dir().join("fp_definitely_not_here.bin");
        assert!(compute_sampled_fingerprint(&missing).is_err());
        assert!(compute_full_sha256(&missing).is_err());
    }
}
