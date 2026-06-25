use std::fs::File;
use std::hash::Hasher;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub fn compute_fnv1a64(path: &Path) -> Result<i64, String> {
    let mut file = File::open(path).map_err(|e| format!("Fingerprint open failed: {}", e))?;

    let metadata = file
        .metadata()
        .map_err(|e| format!("Fingerprint metadata failed: {}", e))?;
    let file_size = metadata.len();

    let mut hasher = fnv::FnvHasher::default();
    hasher.write(&file_size.to_le_bytes());

    let read_limit: usize = 8192;

    if file_size <= (read_limit as u64 * 2) {
        let mut buf = vec![0u8; file_size as usize];
        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("Fingerprint seek failed: {}", e))?;
        file.read_exact(&mut buf)
            .map_err(|e| format!("Fingerprint read failed: {}", e))?;
        hasher.write(&buf);
    } else {
        let mut first_chunk = vec![0u8; read_limit];
        file.seek(SeekFrom::Start(0))
            .map_err(|e| format!("Fingerprint seek failed: {}", e))?;
        file.read_exact(&mut first_chunk)
            .map_err(|e| format!("Fingerprint read failed: {}", e))?;
        hasher.write(&first_chunk);

        let mut last_chunk = vec![0u8; read_limit];
        file.seek(SeekFrom::End(-(read_limit as i64)))
            .map_err(|e| format!("Fingerprint seek failed: {}", e))?;
        file.read_exact(&mut last_chunk)
            .map_err(|e| format!("Fingerprint read failed: {}", e))?;
        hasher.write(&last_chunk);
    }

    Ok(hasher.finish() as i64)
}
