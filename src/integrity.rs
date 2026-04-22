use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::mem::MaybeUninit;
use std::os::fd::RawFd;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

use crate::system::logical_block_size;

const MAX_CHUNK_BYTES: usize = 1024 * 1024;
const BLKGETSIZE64_IOCTL: libc::c_ulong = 0x8008_1272;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChecksumAlgorithm {
    Crc32,
    Md5,
    Sha256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityBaseline {
    pub device: PathBuf,
    pub region_offset_bytes: u64,
    pub region_length_bytes: u64,
    pub algorithm: ChecksumAlgorithm,
    pub checksum_hex: String,
    pub region_size_actual: u64,
    pub captured_at: DateTime<Utc>,
    #[serde(default)]
    pub snapshot_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityVerification {
    pub baseline: IntegrityBaseline,
    pub verified_at: DateTime<Utc>,
    pub verified_checksum_hex: String,
    pub matches: bool,
    pub note: String,
    #[serde(default)]
    pub snapshot_path: Option<PathBuf>,
}

pub fn compute_region_checksum(
    device: &Path,
    offset_bytes: u64,
    length_bytes: u64,
    algorithm: ChecksumAlgorithm,
) -> Result<IntegrityBaseline> {
    let (checksum_hex, region_size_actual) =
        compute_checksum_hex(device, offset_bytes, length_bytes, &algorithm)?;
    Ok(IntegrityBaseline {
        device: device.to_path_buf(),
        region_offset_bytes: offset_bytes,
        region_length_bytes: length_bytes,
        algorithm,
        checksum_hex,
        region_size_actual,
        captured_at: Utc::now(),
        snapshot_path: None,
    })
}

pub fn verify_against_baseline(
    baseline: &IntegrityBaseline,
    note: &str,
) -> Result<IntegrityVerification> {
    let (verified_checksum_hex, _) = compute_checksum_hex(
        &baseline.device,
        baseline.region_offset_bytes,
        baseline.region_length_bytes,
        &baseline.algorithm,
    )?;
    Ok(IntegrityVerification {
        baseline: baseline.clone(),
        verified_at: Utc::now(),
        matches: verified_checksum_hex == baseline.checksum_hex,
        verified_checksum_hex,
        note: note.to_string(),
        snapshot_path: None,
    })
}

/// Copy the selected logical region into an analysis file and return the bytes written.
pub fn capture_region_snapshot(
    device: &Path,
    offset_bytes: u64,
    length_bytes: u64,
    output_path: &Path,
) -> Result<u64> {
    let metadata = std::fs::metadata(device)
        .with_context(|| format!("failed to stat {}", device.display()))?;
    let total_size = if metadata.file_type().is_block_device() {
        block_device_size_bytes(device)?
    } else {
        metadata.len()
    };
    if offset_bytes > total_size {
        bail!(
            "offset {} exceeds device length {} for {}",
            offset_bytes,
            total_size,
            device.display()
        );
    }
    let mut remaining = if length_bytes == 0 {
        total_size.saturating_sub(offset_bytes)
    } else {
        length_bytes.min(total_size.saturating_sub(offset_bytes))
    };
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut input =
        File::open(device).with_context(|| format!("failed to open {}", device.display()))?;
    input
        .seek(SeekFrom::Start(offset_bytes))
        .with_context(|| format!("failed to seek {}", device.display()))?;
    let mut output = File::create(output_path)
        .with_context(|| format!("failed to create {}", output_path.display()))?;
    let mut buffer = vec![0_u8; MAX_CHUNK_BYTES];
    let mut written = 0_u64;
    while remaining > 0 {
        let desired = remaining.min(buffer.len() as u64) as usize;
        let bytes_read = input
            .read(&mut buffer[..desired])
            .with_context(|| format!("failed to read {}", device.display()))?;
        if bytes_read == 0 {
            break;
        }
        output
            .write_all(&buffer[..bytes_read])
            .with_context(|| format!("failed to write {}", output_path.display()))?;
        remaining = remaining.saturating_sub(bytes_read as u64);
        written = written.saturating_add(bytes_read as u64);
    }
    Ok(written)
}

fn compute_checksum_hex(
    device: &Path,
    offset_bytes: u64,
    length_bytes: u64,
    algorithm: &ChecksumAlgorithm,
) -> Result<(String, u64)> {
    let metadata = std::fs::metadata(device)
        .with_context(|| format!("failed to stat {}", device.display()))?;
    let total_size = if metadata.file_type().is_block_device() {
        block_device_size_bytes(device)?
    } else {
        metadata.len()
    };
    if offset_bytes > total_size {
        bail!(
            "offset {} exceeds device length {} for {}",
            offset_bytes,
            total_size,
            device.display()
        );
    }
    let requested_length = if length_bytes == 0 {
        total_size.saturating_sub(offset_bytes)
    } else {
        length_bytes.min(total_size.saturating_sub(offset_bytes))
    };
    if metadata.file_type().is_block_device() {
        compute_direct_checksum_hex(device, offset_bytes, requested_length, algorithm)
    } else {
        compute_buffered_checksum_hex(device, offset_bytes, requested_length, algorithm)
    }
}

fn compute_direct_checksum_hex(
    device: &Path,
    offset_bytes: u64,
    length_bytes: u64,
    algorithm: &ChecksumAlgorithm,
) -> Result<(String, u64)> {
    let alignment = logical_block_size(device).unwrap_or(512).max(512) as usize;
    if offset_bytes % alignment as u64 != 0 {
        bail!(
            "offset {} is not aligned to logical block size {} for {}",
            offset_bytes,
            alignment,
            device.display()
        );
    }
    let c_path = CString::new(device.to_string_lossy().to_string())?;
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY | o_direct_flag(), 0) };
    if fd < 0 {
        bail!(
            "failed to open {}: {}",
            device.display(),
            std::io::Error::last_os_error()
        );
    }
    let result = compute_checksum_from_fd(fd, offset_bytes, length_bytes, alignment, algorithm);
    unsafe {
        libc::close(fd);
    }
    result
}

fn compute_checksum_from_fd(
    fd: RawFd,
    offset_bytes: u64,
    length_bytes: u64,
    alignment: usize,
    algorithm: &ChecksumAlgorithm,
) -> Result<(String, u64)> {
    let mut buffer = AlignedBuffer::new(MAX_CHUNK_BYTES, alignment)?;
    let mut checksum = ChecksumState::new(algorithm);
    let mut remaining = length_bytes;
    let mut file_offset = offset_bytes;

    while remaining > 0 {
        let desired = remaining.min(MAX_CHUNK_BYTES as u64) as usize;
        let read_len = align_up(desired, alignment);
        let rc = unsafe {
            libc::pread(
                fd,
                buffer.as_mut_ptr().cast(),
                read_len,
                file_offset as libc::off_t,
            )
        };
        if rc < 0 {
            bail!("{}", std::io::Error::last_os_error());
        }
        if rc == 0 {
            break;
        }
        let bytes_read = rc as usize;
        let useful = bytes_read.min(desired);
        checksum.update(&buffer.as_slice()[..useful]);
        remaining = remaining.saturating_sub(useful as u64);
        file_offset = file_offset.saturating_add(useful as u64);
        if bytes_read < read_len {
            break;
        }
    }

    let region_size_actual = length_bytes.saturating_sub(remaining);
    Ok((checksum.finalize_hex(), region_size_actual))
}

fn compute_buffered_checksum_hex(
    device: &Path,
    offset_bytes: u64,
    length_bytes: u64,
    algorithm: &ChecksumAlgorithm,
) -> Result<(String, u64)> {
    let mut file =
        File::open(device).with_context(|| format!("failed to open {}", device.display()))?;
    file.seek(SeekFrom::Start(offset_bytes))
        .with_context(|| format!("failed to seek {}", device.display()))?;
    let mut checksum = ChecksumState::new(algorithm);
    let mut remaining = length_bytes;
    let mut buffer = vec![0_u8; MAX_CHUNK_BYTES];
    while remaining > 0 {
        let desired = remaining.min(buffer.len() as u64) as usize;
        let bytes_read = file
            .read(&mut buffer[..desired])
            .with_context(|| format!("failed to read {}", device.display()))?;
        if bytes_read == 0 {
            break;
        }
        checksum.update(&buffer[..bytes_read]);
        remaining = remaining.saturating_sub(bytes_read as u64);
    }
    let region_size_actual = length_bytes.saturating_sub(remaining);
    Ok((checksum.finalize_hex(), region_size_actual))
}

fn block_device_size_bytes(device: &Path) -> Result<u64> {
    let c_path = CString::new(device.to_string_lossy().to_string())?;
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY, 0) };
    if fd < 0 {
        bail!(
            "failed to open {}: {}",
            device.display(),
            std::io::Error::last_os_error()
        );
    }
    let mut size = 0_u64;
    let rc = unsafe { libc::ioctl(fd, BLKGETSIZE64_IOCTL, &mut size) };
    unsafe {
        libc::close(fd);
    }
    if rc < 0 {
        bail!(
            "BLKGETSIZE64 failed for {}: {}",
            device.display(),
            std::io::Error::last_os_error()
        );
    }
    Ok(size)
}

fn align_up(value: usize, alignment: usize) -> usize {
    if value == 0 {
        0
    } else {
        value.div_ceil(alignment) * alignment
    }
}

fn o_direct_flag() -> i32 {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        libc::O_DIRECT
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        0
    }
}

enum ChecksumState {
    Crc32(crc32fast::Hasher),
    Md5(Md5),
    Sha256(Sha256),
}

impl ChecksumState {
    fn new(algorithm: &ChecksumAlgorithm) -> Self {
        match algorithm {
            ChecksumAlgorithm::Crc32 => Self::Crc32(crc32fast::Hasher::new()),
            ChecksumAlgorithm::Md5 => Self::Md5(Md5::new()),
            ChecksumAlgorithm::Sha256 => Self::Sha256(Sha256::new()),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Crc32(state) => state.update(bytes),
            Self::Md5(state) => state.update(bytes),
            Self::Sha256(state) => state.update(bytes),
        }
    }

    fn finalize_hex(self) -> String {
        match self {
            Self::Crc32(state) => format!("{:08x}", state.finalize()),
            Self::Md5(state) => format!("{:x}", state.finalize()),
            Self::Sha256(state) => format!("{:x}", state.finalize()),
        }
    }
}

struct AlignedBuffer {
    ptr: *mut u8,
    len: usize,
}

impl AlignedBuffer {
    fn new(len: usize, align: usize) -> Result<Self> {
        let mut ptr = MaybeUninit::<*mut libc::c_void>::uninit();
        let rc = unsafe { libc::posix_memalign(ptr.as_mut_ptr(), align.max(512), len) };
        if rc != 0 {
            bail!("posix_memalign failed with {}", rc);
        }
        let ptr = unsafe { ptr.assume_init() }.cast::<u8>();
        unsafe {
            std::ptr::write_bytes(ptr, 0, len);
        }
        Ok(Self { ptr, len })
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        unsafe {
            libc::free(self.ptr.cast());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_region_checksum, verify_against_baseline, ChecksumAlgorithm};
    use std::fs;
    use tempfile::NamedTempFile;

    #[test]
    fn test_checksum_algorithm_round_trip() {
        let file = NamedTempFile::new().expect("tempfile");
        fs::write(file.path(), b"abc").expect("seed file");

        let crc32 = compute_region_checksum(file.path(), 0, 3, ChecksumAlgorithm::Crc32)
            .expect("crc32 baseline");
        let md5 = compute_region_checksum(file.path(), 0, 3, ChecksumAlgorithm::Md5)
            .expect("md5 baseline");
        let sha256 = compute_region_checksum(file.path(), 0, 3, ChecksumAlgorithm::Sha256)
            .expect("sha256 baseline");

        assert_eq!(crc32.checksum_hex, "352441c2");
        assert_eq!(md5.checksum_hex, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            sha256.checksum_hex,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn test_verify_detects_mismatch() {
        let file = NamedTempFile::new().expect("tempfile");
        fs::write(file.path(), b"abcdefgh").expect("seed file");

        let baseline = compute_region_checksum(file.path(), 0, 8, ChecksumAlgorithm::Sha256)
            .expect("baseline");
        fs::write(file.path(), b"abcDefgh").expect("mutate file");

        let verification =
            verify_against_baseline(&baseline, "modified").expect("verification result");
        assert!(!verification.matches);
        assert_ne!(
            verification.verified_checksum_hex,
            verification.baseline.checksum_hex
        );
    }
}
