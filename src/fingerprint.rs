use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const CHUNK_SIZE: usize = 64 * 1024;

pub fn compute_fnv1a64(path: &Path) -> Result<i64, String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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

        let fp1 = compute_fnv1a64(&path1).unwrap();
        let fp2 = compute_fnv1a64(&path2).unwrap();
        assert_eq!(fp1, fp2, "Identical content should produce identical fingerprints");

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

        let fp_a = compute_fnv1a64(&path_a).unwrap();
        let fp_b = compute_fnv1a64(&path_b).unwrap();
        assert_ne!(fp_a, fp_b, "Different content should produce different fingerprints");

        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }
}
