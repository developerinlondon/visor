use super::*;

#[test]
fn create_and_drop_succeeds() {
    let name = "/visor-test-create-drop";
    let region = SharedMemoryRegion::create(name, 4096).unwrap();
    assert!(!region.as_ptr().is_null());
    assert_eq!(region.size(), 4096);
    assert_eq!(region.name(), name);
    assert!(region.fd() >= 0);
    region.unlink().unwrap();
}

#[test]
fn write_read_round_trip_through_shared_memory() {
    let name = "/visor-test-write-read";
    let region = SharedMemoryRegion::create(name, 4096).unwrap();

    let data = b"hello shared memory";
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), region.as_ptr(), data.len());
    }

    let mut buf = vec![0u8; data.len()];
    unsafe {
        std::ptr::copy_nonoverlapping(region.as_ptr(), buf.as_mut_ptr(), data.len());
    }
    assert_eq!(buf, data);
    region.unlink().unwrap();
}

#[test]
fn mmap_shared_fd_maps_same_memory() {
    let name = "/visor-test-mmap-fd";
    let region = SharedMemoryRegion::create(name, 4096).unwrap();

    let data = b"shared across mappings";
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), region.as_ptr(), data.len());
    }

    let second_ptr = mmap_shared_fd(region.fd(), 4096).unwrap();

    let mut buf = vec![0u8; data.len()];
    unsafe {
        std::ptr::copy_nonoverlapping(second_ptr, buf.as_mut_ptr(), data.len());
    }
    assert_eq!(buf, data);

    unsafe {
        libc::munmap(second_ptr.cast(), 4096);
    }
    region.unlink().unwrap();
}

#[test]
fn unlink_removes_shm_object() {
    let name = "/visor-test-unlink";
    let region = SharedMemoryRegion::create(name, 4096).unwrap();
    region.unlink().unwrap();

    let result = SharedMemoryRegion::create(name, 4096);
    assert!(result.is_ok());
    result.unwrap().unlink().unwrap();
}

#[test]
fn unlink_shared_memory_ignores_enoent() {
    let result = unlink_shared_memory("/visor-test-nonexistent-shm");
    assert!(result.is_ok());
}

#[test]
fn invalid_name_with_null_byte_returns_error() {
    let result = SharedMemoryRegion::create("/visor-test\0bad", 4096);
    assert!(result.is_err());
    match result.unwrap_err() {
        SharedMemoryError::InvalidName { name } => {
            assert_eq!(name, "/visor-test\0bad");
        }
        other => panic!("expected InvalidName, got: {other}"),
    }
}

#[test]
fn mmap_shared_fd_with_invalid_fd_returns_error() {
    let result = mmap_shared_fd(-1, 4096);
    assert!(result.is_err());
}

#[test]
fn shared_memory_error_converts_to_memory_error() {
    let shm_err = SharedMemoryError::MmapFd {
        fd: -1,
        size: 4096,
        source: std::io::Error::from_raw_os_error(libc::ENOMEM),
    };
    let mem_err: MemoryError = shm_err.into();
    assert!(matches!(
        mem_err,
        MemoryError::Allocation { size: 4096, .. }
    ));
}

#[test]
fn large_region_is_demand_paged() {
    let name = "/visor-test-large-region";
    let size = 256 * 1024 * 1024; // 256 MiB
    let region = SharedMemoryRegion::create(name, size).unwrap();
    assert_eq!(region.size(), size);

    unsafe {
        *region.as_ptr() = 42;
        *region.as_ptr().add(size - 1) = 99;
    }

    unsafe {
        assert_eq!(*region.as_ptr(), 42);
        assert_eq!(*region.as_ptr().add(size - 1), 99);
    }

    region.unlink().unwrap();
}
