//! VM snapshot save and restore.
//!
//! Captures full VM state (vCPU registers + guest memory) to disk and restores
//! it via `mmap(MAP_PRIVATE)` for copy-on-write fast restore. Multiple VMs
//! restored from the same snapshot share physical pages until written.
//!
//! # Snapshot Format
//!
//! ```text
//! <snapshot_dir>/
//!   memory.bin       Guest RAM (raw bytes, file size == memory size)
//!   cpu_state.json   vCPU registers serialized as JSON
//! ```
//!
//! # Restore Performance
//!
//! Restore is O(1) regardless of memory size — `mmap` creates page table
//! entries lazily. A 10 GiB snapshot restores in <1ms.

#![allow(unsafe_code)]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::ptr;

#[cfg(target_os = "linux")]
use kvm_bindings::{kvm_fpu, kvm_regs, kvm_sregs};
#[cfg(target_os = "linux")]
use kvm_ioctls::VcpuFd;

use crate::memory::{GuestMemory, MemoryError};

/// Errors from snapshot operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SnapshotError {
    /// Failed to read vCPU registers.
    #[error("get registers failed: {0}")]
    GetRegs(std::io::Error),

    /// Failed to write vCPU registers.
    #[error("set registers failed: {0}")]
    SetRegs(std::io::Error),

    /// I/O error during snapshot save or restore.
    #[error("snapshot I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Memory file size does not match expected guest memory size.
    #[error("memory size mismatch: expected {expected} bytes, file has {actual} bytes")]
    MemoryMismatch {
        /// Expected size from snapshot metadata.
        expected: usize,
        /// Actual file size on disk.
        actual: usize,
    },

    /// Memory allocation failed during restore.
    #[error("memory restore failed: {0}")]
    Memory(#[from] MemoryError),
}

#[cfg(target_os = "linux")]
/// Saved vCPU register state.
///
/// Contains general-purpose registers, special registers (segment, control),
/// and FPU/SSE state. Sufficient to resume a paused vCPU.
#[non_exhaustive]
pub struct CpuSnapshot {
    /// General-purpose registers (RAX, RBX, ... , RIP, RFLAGS).
    pub regs: kvm_regs,
    /// Special registers (segments, CR0-CR4, EFER, IDT, GDT).
    pub sregs: kvm_sregs,
    /// FPU and SSE state (x87, XMM, MXCSR).
    pub fpu: kvm_fpu,
}

#[cfg(target_os = "macos")]
/// Saved vCPU register state for ARM64.
///
/// Contains general-purpose registers (X0\u{2013}X30, SP, PC, CPSR) and
/// EL1 system registers. Sufficient to resume a paused vCPU.
#[non_exhaustive]
pub struct CpuSnapshot {
    /// General-purpose registers.
    pub regs: crate::platform::regs::StandardRegs,
    /// System registers (EL1).
    pub sregs: crate::platform::regs::SpecialRegs,
}

#[cfg(target_os = "linux")]
/// Full VM snapshot bundle.
///
/// Combines CPU state, memory file path, and a placeholder for device
/// state. The memory file is a raw dump of guest RAM that can be restored
/// via `mmap(MAP_PRIVATE)` for copy-on-write sharing.
#[non_exhaustive]
pub struct SnapshotBundle {
    /// Saved vCPU register state.
    pub cpu: CpuSnapshot,
    /// Path to the guest memory dump file.
    pub memory_path: PathBuf,
    /// Guest memory size in bytes.
    pub memory_size: usize,
    /// Guest physical base address.
    pub guest_base: u64,
    /// Placeholder for serialized device state (virtio queues, etc.).
    pub device_state: Vec<u8>,
}

#[cfg(target_os = "linux")]
/// Saves vCPU register state.
///
/// Reads general-purpose registers, special registers, and FPU state
/// from the KVM vCPU file descriptor.
///
/// # Errors
///
/// Returns [`SnapshotError::GetRegs`] if any KVM ioctl fails.
pub fn save_cpu(vcpu_fd: &VcpuFd) -> Result<CpuSnapshot, SnapshotError> {
    let regs = vcpu_fd
        .get_regs()
        .map_err(|e| SnapshotError::GetRegs(std::io::Error::from_raw_os_error(e.errno())))?;
    let sregs = vcpu_fd
        .get_sregs()
        .map_err(|e| SnapshotError::GetRegs(std::io::Error::from_raw_os_error(e.errno())))?;
    let fpu = vcpu_fd
        .get_fpu()
        .map_err(|e| SnapshotError::GetRegs(std::io::Error::from_raw_os_error(e.errno())))?;

    Ok(CpuSnapshot { regs, sregs, fpu })
}

#[cfg(target_os = "linux")]
/// Restores vCPU register state from a snapshot.
///
/// Writes general-purpose registers, special registers, and FPU state
/// back to the KVM vCPU file descriptor.
///
/// # Errors
///
/// Returns [`SnapshotError::SetRegs`] if any KVM ioctl fails.
pub fn restore_cpu(vcpu_fd: &VcpuFd, snap: &CpuSnapshot) -> Result<(), SnapshotError> {
    vcpu_fd
        .set_regs(&snap.regs)
        .map_err(|e| SnapshotError::SetRegs(std::io::Error::from_raw_os_error(e.errno())))?;
    vcpu_fd
        .set_sregs(&snap.sregs)
        .map_err(|e| SnapshotError::SetRegs(std::io::Error::from_raw_os_error(e.errno())))?;
    vcpu_fd
        .set_fpu(&snap.fpu)
        .map_err(|e| SnapshotError::SetRegs(std::io::Error::from_raw_os_error(e.errno())))?;

    Ok(())
}

#[cfg(target_os = "macos")]
/// Saves vCPU register state.
///
/// Reads general-purpose registers and system registers from the vCPU.
///
/// # Errors
///
/// Returns [`SnapshotError::GetRegs`] if register read fails.
pub fn save_cpu(vcpu: &impl crate::platform::VcpuOps) -> Result<CpuSnapshot, SnapshotError> {
    let regs = vcpu
        .get_regs()
        .map_err(|e| SnapshotError::GetRegs(std::io::Error::other(e.to_string())))?;
    let sregs = vcpu
        .get_sregs()
        .map_err(|e| SnapshotError::GetRegs(std::io::Error::other(e.to_string())))?;
    Ok(CpuSnapshot { regs, sregs })
}

#[cfg(target_os = "macos")]
/// Restores vCPU register state from a snapshot.
///
/// Writes general-purpose registers and system registers back to the vCPU.
///
/// # Errors
///
/// Returns [`SnapshotError::SetRegs`] if register write fails.
pub fn restore_cpu(
    vcpu: &impl crate::platform::VcpuOps,
    snap: &CpuSnapshot,
) -> Result<(), SnapshotError> {
    vcpu.set_regs(&snap.regs)
        .map_err(|e| SnapshotError::SetRegs(std::io::Error::other(e.to_string())))?;
    vcpu.set_sregs(&snap.sregs)
        .map_err(|e| SnapshotError::SetRegs(std::io::Error::other(e.to_string())))?;
    Ok(())
}

/// Saves guest memory to a file.
///
/// Writes the raw guest RAM region as a contiguous binary file. The file
/// size equals `memory.size()` bytes.
///
/// # Errors
///
/// Returns [`SnapshotError::Io`] if file creation or write fails.
pub fn save_memory(memory: &GuestMemory, path: &Path) -> Result<(), SnapshotError> {
    let size = memory.size();
    // SAFETY: GuestMemory guarantees host_addr points to a valid mmap region
    // of exactly `size` bytes. We only read from it (immutable slice).
    let data = unsafe { std::slice::from_raw_parts(memory.host_addr(), size) };

    let mut file = fs::File::create(path)?;
    file.write_all(data)?;
    file.sync_all()?;

    Ok(())
}

/// Restores guest memory from a snapshot file using `mmap(MAP_PRIVATE)`.
///
/// The restored memory shares physical pages with the file via copy-on-write.
/// Multiple VMs restored from the same file share pages until either writes
/// to them. This makes restore O(1) regardless of memory size.
///
/// # Errors
///
/// Returns [`SnapshotError::MemoryMismatch`] if the file size doesn't match
/// `expected_size`, or [`SnapshotError::Io`] if mmap fails.
pub fn restore_memory(
    path: &Path,
    expected_size: usize,
    guest_base: u64,
) -> Result<GuestMemory, SnapshotError> {
    let file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    let file_size = usize::try_from(metadata.len()).unwrap_or(usize::MAX);

    if file_size != expected_size {
        return Err(SnapshotError::MemoryMismatch {
            expected: expected_size,
            actual: file_size,
        });
    }
    let fd = {
        use std::os::unix::io::AsRawFd;
        file.as_raw_fd()
    };

    // SAFETY: We mmap the file with MAP_PRIVATE (copy-on-write).
    // - fd is a valid file descriptor from an open File
    // - expected_size matches the file size (checked above)
    // - MAP_PRIVATE ensures writes don't affect the original file
    // - PROT_READ | PROT_WRITE allows the guest to read and write memory
    // - We check for MAP_FAILED before using the pointer
    let addr = unsafe {
        libc::mmap(
            ptr::null_mut(),
            expected_size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_NORESERVE,
            fd,
            0,
        )
    };

    if addr == libc::MAP_FAILED {
        return Err(SnapshotError::Io(std::io::Error::last_os_error()));
    }

    // Construct GuestMemory from the mmap'd file region.
    // We use from_raw_mmap which takes ownership of the mapping.
    Ok(GuestMemory::from_raw_mmap(
        addr.cast::<u8>(),
        expected_size,
        guest_base,
    ))
}

#[cfg(target_os = "linux")]
/// Saves a complete VM snapshot to a directory.
///
/// Creates `memory.bin` and `cpu_state.json` in the given directory.
/// The directory must exist.
///
/// # Errors
///
/// Returns a [`SnapshotError`] if any save step fails.
pub fn save_bundle(
    vcpu_fd: &VcpuFd,
    memory: &GuestMemory,
    snapshot_dir: &Path,
    device_state: Vec<u8>,
) -> Result<SnapshotBundle, SnapshotError> {
    let memory_path = snapshot_dir.join("memory.bin");
    let cpu_path = snapshot_dir.join("cpu_state.json");

    let cpu = save_cpu(vcpu_fd)?;
    save_memory(memory, &memory_path)?;

    // Serialize CPU state as JSON for portability and debuggability.
    // kvm_regs/sregs/fpu are repr(C) but we serialize field-by-field.
    let cpu_json = serialize_cpu_state(&cpu);
    fs::write(&cpu_path, cpu_json)?;

    if !device_state.is_empty() {
        let device_path = snapshot_dir.join("device_state.bin");
        fs::write(device_path, &device_state)?;
    }

    Ok(SnapshotBundle {
        cpu,
        memory_path,
        memory_size: memory.size(),
        guest_base: memory.guest_base(),
        device_state,
    })
}

#[cfg(target_os = "linux")]
/// Restores a complete VM snapshot from a directory.
///
/// Reads `memory.bin` via `mmap(MAP_PRIVATE)` and `cpu_state.json`,
/// then restores vCPU registers.
///
/// # Errors
///
/// Returns a [`SnapshotError`] if any restore step fails.
pub fn restore_bundle(
    vcpu_fd: &VcpuFd,
    snapshot_dir: &Path,
    memory_size: usize,
    guest_base: u64,
) -> Result<(GuestMemory, Vec<u8>), SnapshotError> {
    let memory_path = snapshot_dir.join("memory.bin");
    let cpu_path = snapshot_dir.join("cpu_state.json");

    let memory = restore_memory(&memory_path, memory_size, guest_base)?;

    let cpu_json = fs::read_to_string(&cpu_path)?;
    let cpu = deserialize_cpu_state(&cpu_json)?;
    restore_cpu(vcpu_fd, &cpu)?;

    let device_path = snapshot_dir.join("device_state.bin");
    let device_state = if device_path.exists() {
        fs::read(device_path)?
    } else {
        Vec::new()
    };

    Ok((memory, device_state))
}

#[cfg(target_os = "macos")]
/// Full VM snapshot bundle.
///
/// Combines CPU state, memory file path, and device state.
#[non_exhaustive]
pub struct SnapshotBundle {
    /// Saved vCPU register state.
    pub cpu: CpuSnapshot,
    /// Path to the guest memory dump file.
    pub memory_path: PathBuf,
    /// Guest memory size in bytes.
    pub memory_size: usize,
    /// Guest physical base address.
    pub guest_base: u64,
    /// Serialized device state (virtio queues, muxer port, etc.).
    pub device_state: Vec<u8>,
}

#[cfg(target_os = "macos")]
/// Saves a complete VM snapshot to a directory.
///
/// Creates `memory.bin` and `cpu_state.json` in the given directory.
/// The directory must exist.
///
/// # Errors
///
/// Returns a [`SnapshotError`] if any save step fails.
pub fn save_bundle(
    vcpu: &impl crate::platform::VcpuOps,
    memory: &GuestMemory,
    snapshot_dir: &Path,
    device_state: Vec<u8>,
) -> Result<SnapshotBundle, SnapshotError> {
    let memory_path = snapshot_dir.join("memory.bin");
    let cpu_path = snapshot_dir.join("cpu_state.json");

    let cpu = save_cpu(vcpu)?;
    save_memory(memory, &memory_path)?;

    let cpu_json = serialize_cpu_state(&cpu);
    fs::write(&cpu_path, cpu_json)?;

    if !device_state.is_empty() {
        let device_path = snapshot_dir.join("device_state.bin");
        fs::write(device_path, &device_state)?;
    }

    Ok(SnapshotBundle {
        cpu,
        memory_path,
        memory_size: memory.size(),
        guest_base: memory.guest_base(),
        device_state,
    })
}

#[cfg(target_os = "macos")]
/// Restores a complete VM snapshot from a directory.
///
/// Reads `memory.bin` via `mmap(MAP_PRIVATE)` and `cpu_state.json`,
/// then restores vCPU registers.
///
/// # Errors
///
/// Returns a [`SnapshotError`] if any restore step fails.
pub fn restore_bundle(
    vcpu: &impl crate::platform::VcpuOps,
    snapshot_dir: &Path,
    memory_size: usize,
    guest_base: u64,
) -> Result<(GuestMemory, Vec<u8>), SnapshotError> {
    let memory_path = snapshot_dir.join("memory.bin");
    let cpu_path = snapshot_dir.join("cpu_state.json");

    let memory = restore_memory(&memory_path, memory_size, guest_base)?;

    let cpu_json = fs::read_to_string(&cpu_path)?;
    let cpu = deserialize_cpu_state(&cpu_json)?;
    restore_cpu(vcpu, &cpu)?;

    let device_path = snapshot_dir.join("device_state.bin");
    let device_state = if device_path.exists() {
        fs::read(device_path)?
    } else {
        Vec::new()
    };

    Ok((memory, device_state))
}

#[cfg(target_os = "linux")]
/// Serializes CPU state to a JSON string.
///
/// Uses raw byte representation of the repr(C) KVM structs encoded as
/// hex strings for portability.
fn serialize_cpu_state(cpu: &CpuSnapshot) -> String {
    // SAFETY: kvm_regs, kvm_sregs, kvm_fpu are repr(C) plain-old-data structs.
    // Converting them to byte slices for hex encoding is safe.
    let regs_bytes = unsafe {
        std::slice::from_raw_parts(
            std::ptr::from_ref(&cpu.regs).cast::<u8>(),
            std::mem::size_of::<kvm_regs>(),
        )
    };
    let sregs_bytes = unsafe {
        std::slice::from_raw_parts(
            std::ptr::from_ref(&cpu.sregs).cast::<u8>(),
            std::mem::size_of::<kvm_sregs>(),
        )
    };
    let fpu_bytes = unsafe {
        std::slice::from_raw_parts(
            std::ptr::from_ref(&cpu.fpu).cast::<u8>(),
            std::mem::size_of::<kvm_fpu>(),
        )
    };

    format!(
        "{{\"regs\":\"{}\",\"sregs\":\"{}\",\"fpu\":\"{}\"}}",
        hex_encode(regs_bytes),
        hex_encode(sregs_bytes),
        hex_encode(fpu_bytes),
    )
}

#[cfg(target_os = "linux")]
/// Deserializes CPU state from a JSON string.
///
/// # Errors
///
/// Returns [`SnapshotError::Io`] if the JSON format is invalid.
pub fn deserialize_cpu_state(json: &str) -> Result<CpuSnapshot, SnapshotError> {
    // Simple JSON parser for our known format.
    let regs_hex = extract_json_field(json, "regs")?;
    let sregs_hex = extract_json_field(json, "sregs")?;
    let fpu_hex = extract_json_field(json, "fpu")?;

    let regs_bytes = hex_decode(&regs_hex)?;
    let sregs_bytes = hex_decode(&sregs_hex)?;
    let fpu_bytes = hex_decode(&fpu_hex)?;

    if regs_bytes.len() != std::mem::size_of::<kvm_regs>() {
        return Err(SnapshotError::Io(std::io::Error::other(format!(
            "regs size mismatch: expected {}, got {}",
            std::mem::size_of::<kvm_regs>(),
            regs_bytes.len()
        ))));
    }
    if sregs_bytes.len() != std::mem::size_of::<kvm_sregs>() {
        return Err(SnapshotError::Io(std::io::Error::other(format!(
            "sregs size mismatch: expected {}, got {}",
            std::mem::size_of::<kvm_sregs>(),
            sregs_bytes.len()
        ))));
    }
    if fpu_bytes.len() != std::mem::size_of::<kvm_fpu>() {
        return Err(SnapshotError::Io(std::io::Error::other(format!(
            "fpu size mismatch: expected {}, got {}",
            std::mem::size_of::<kvm_fpu>(),
            fpu_bytes.len()
        ))));
    }

    // SAFETY: We verified the byte lengths match the struct sizes.
    // kvm_regs, kvm_sregs, kvm_fpu are repr(C) POD types with no
    // padding requirements beyond natural alignment, and any bit
    // pattern is valid for them.
    let regs = unsafe { std::ptr::read_unaligned(regs_bytes.as_ptr().cast::<kvm_regs>()) };
    let sregs = unsafe { std::ptr::read_unaligned(sregs_bytes.as_ptr().cast::<kvm_sregs>()) };
    let fpu = unsafe { std::ptr::read_unaligned(fpu_bytes.as_ptr().cast::<kvm_fpu>()) };

    Ok(CpuSnapshot { regs, sregs, fpu })
}

/// Extracts a string value from a simple JSON object by field name.
#[cfg(target_os = "linux")]
fn extract_json_field(json: &str, field: &str) -> Result<String, SnapshotError> {
    let pattern = format!("\"{field}\":\"");
    let start = json
        .find(&pattern)
        .ok_or_else(|| std::io::Error::other(format!("missing field: {field}")))?
        + pattern.len();
    let end = json[start..]
        .find('"')
        .ok_or_else(|| std::io::Error::other(format!("unterminated field: {field}")))?
        + start;
    Ok(json[start..end].to_owned())
}

/// Hex-encodes a byte slice.
#[cfg(target_os = "linux")]
fn hex_encode(data: &[u8]) -> String {
    use std::fmt::Write;
    data.iter()
        .fold(String::with_capacity(data.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Hex-decodes a string into bytes.
#[cfg(target_os = "linux")]
fn hex_decode(hex: &str) -> Result<Vec<u8>, SnapshotError> {
    if hex.len() % 2 != 0 {
        return Err(SnapshotError::Io(std::io::Error::other(
            "odd-length hex string",
        )));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|e| SnapshotError::Io(std::io::Error::other(e.to_string())))
        })
        .collect()
}

#[cfg(target_os = "macos")]
/// Serializes ARM64 CPU state to a JSON string.
///
/// Each register is stored as a named field for debuggability.
fn serialize_cpu_state(cpu: &CpuSnapshot) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(2048);
    s.push_str("{\"regs\":{\"x\":[");
    for (i, val) in cpu.regs.x.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let _ = write!(s, "{val}");
    }
    s.push_str("],");
    let _ = write!(s, "\"sp\":{},", cpu.regs.sp);
    let _ = write!(s, "\"pc\":{},", cpu.regs.pc);
    let _ = write!(s, "\"cpsr\":{},", cpu.regs.cpsr);
    let _ = write!(s, "\"fpcr\":{},", cpu.regs.fpcr);
    let _ = write!(s, "\"fpsr\":{}", cpu.regs.fpsr);
    s.push_str("},\"sregs\":{");
    let _ = write!(s, "\"sctlr_el1\":{},", cpu.sregs.sctlr_el1);
    let _ = write!(s, "\"ttbr0_el1\":{},", cpu.sregs.ttbr0_el1);
    let _ = write!(s, "\"ttbr1_el1\":{},", cpu.sregs.ttbr1_el1);
    let _ = write!(s, "\"tcr_el1\":{},", cpu.sregs.tcr_el1);
    let _ = write!(s, "\"mair_el1\":{},", cpu.sregs.mair_el1);
    let _ = write!(s, "\"vbar_el1\":{},", cpu.sregs.vbar_el1);
    let _ = write!(s, "\"spsr_el1\":{},", cpu.sregs.spsr_el1);
    let _ = write!(s, "\"elr_el1\":{},", cpu.sregs.elr_el1);
    let _ = write!(s, "\"sp_el0\":{},", cpu.sregs.sp_el0);
    let _ = write!(s, "\"sp_el1\":{},", cpu.sregs.sp_el1);
    let _ = write!(s, "\"esr_el1\":{},", cpu.sregs.esr_el1);
    let _ = write!(s, "\"far_el1\":{},", cpu.sregs.far_el1);
    let _ = write!(s, "\"par_el1\":{},", cpu.sregs.par_el1);
    let _ = write!(s, "\"cpacr_el1\":{},", cpu.sregs.cpacr_el1);
    let _ = write!(s, "\"cntkctl_el1\":{},", cpu.sregs.cntkctl_el1);
    let _ = write!(s, "\"cntv_ctl_el0\":{},", cpu.sregs.cntv_ctl_el0);
    let _ = write!(s, "\"cntv_cval_el0\":{},", cpu.sregs.cntv_cval_el0);
    let _ = write!(s, "\"tpidr_el0\":{},", cpu.sregs.tpidr_el0);
    let _ = write!(s, "\"tpidrro_el0\":{},", cpu.sregs.tpidrro_el0);
    let _ = write!(s, "\"tpidr_el1\":{},", cpu.sregs.tpidr_el1);
    let _ = write!(s, "\"contextidr_el1\":{},", cpu.sregs.contextidr_el1);
    let _ = write!(s, "\"amair_el1\":{},", cpu.sregs.amair_el1);
    let _ = write!(s, "\"afsr0_el1\":{},", cpu.sregs.afsr0_el1);
    let _ = write!(s, "\"afsr1_el1\":{},", cpu.sregs.afsr1_el1);
    let _ = write!(s, "\"midr_el1\":{},", cpu.sregs.midr_el1);
    let _ = write!(s, "\"mpidr_el1\":{}", cpu.sregs.mpidr_el1);
    s.push_str("}}");
    s
}

#[cfg(target_os = "macos")]
/// Deserializes ARM64 CPU state from a JSON string.
///
/// # Errors
///
/// Returns [`SnapshotError::Io`] if the JSON format is invalid.
pub fn deserialize_cpu_state(json: &str) -> Result<CpuSnapshot, SnapshotError> {
    use crate::platform::regs::{SpecialRegs, StandardRegs};

    fn parse_u64(json: &str, field: &str) -> Result<u64, SnapshotError> {
        let pattern = format!("\"{field}\":");
        let start = json
            .find(&pattern)
            .ok_or_else(|| std::io::Error::other(format!("missing field: {field}")))?
            + pattern.len();
        let rest = json[start..].trim_start();
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        rest[..end]
            .parse::<u64>()
            .map_err(|e| SnapshotError::Io(std::io::Error::other(format!("bad {field}: {e}"))))
    }

    fn parse_u64_array(json: &str, field: &str, len: usize) -> Result<Vec<u64>, SnapshotError> {
        let pattern = format!("\"{field}\":[");
        let start = json
            .find(&pattern)
            .ok_or_else(|| std::io::Error::other(format!("missing field: {field}")))?
            + pattern.len();
        let end = json[start..]
            .find(']')
            .ok_or_else(|| std::io::Error::other(format!("unterminated array: {field}")))?
            + start;
        let values: Result<Vec<u64>, _> = json[start..end]
            .split(',')
            .map(|s| {
                s.trim().parse::<u64>().map_err(|e| {
                    SnapshotError::Io(std::io::Error::other(format!("bad {field} element: {e}")))
                })
            })
            .collect();
        let values = values?;
        if values.len() != len {
            return Err(SnapshotError::Io(std::io::Error::other(format!(
                "{field} array length mismatch: expected {len}, got {}",
                values.len()
            ))));
        }
        Ok(values)
    }

    let x_vec = parse_u64_array(json, "x", 31)?;
    let mut x = [0u64; 31];
    x.copy_from_slice(&x_vec);

    let regs = StandardRegs {
        x,
        sp: parse_u64(json, "sp")?,
        pc: parse_u64(json, "pc")?,
        cpsr: parse_u64(json, "cpsr")?,
        fpcr: parse_u64(json, "fpcr")?,
        fpsr: parse_u64(json, "fpsr")?,
    };

    let sregs = SpecialRegs {
        sctlr_el1: parse_u64(json, "sctlr_el1")?,
        ttbr0_el1: parse_u64(json, "ttbr0_el1")?,
        ttbr1_el1: parse_u64(json, "ttbr1_el1")?,
        tcr_el1: parse_u64(json, "tcr_el1")?,
        mair_el1: parse_u64(json, "mair_el1")?,
        vbar_el1: parse_u64(json, "vbar_el1")?,
        spsr_el1: parse_u64(json, "spsr_el1")?,
        elr_el1: parse_u64(json, "elr_el1")?,
        sp_el0: parse_u64(json, "sp_el0")?,
        sp_el1: parse_u64(json, "sp_el1")?,
        esr_el1: parse_u64(json, "esr_el1")?,
        far_el1: parse_u64(json, "far_el1")?,
        par_el1: parse_u64(json, "par_el1")?,
        cpacr_el1: parse_u64(json, "cpacr_el1")?,
        cntkctl_el1: parse_u64(json, "cntkctl_el1")?,
        cntv_ctl_el0: parse_u64(json, "cntv_ctl_el0")?,
        cntv_cval_el0: parse_u64(json, "cntv_cval_el0")?,
        tpidr_el0: parse_u64(json, "tpidr_el0")?,
        tpidrro_el0: parse_u64(json, "tpidrro_el0")?,
        tpidr_el1: parse_u64(json, "tpidr_el1")?,
        contextidr_el1: parse_u64(json, "contextidr_el1")?,
        amair_el1: parse_u64(json, "amair_el1")?,
        afsr0_el1: parse_u64(json, "afsr0_el1")?,
        afsr1_el1: parse_u64(json, "afsr1_el1")?,
        midr_el1: parse_u64(json, "midr_el1")?,
        mpidr_el1: parse_u64(json, "mpidr_el1")?,
    };

    Ok(CpuSnapshot { regs, sregs })
}

#[cfg(test)]
#[path = "snapshot_test.rs"]
mod tests;
