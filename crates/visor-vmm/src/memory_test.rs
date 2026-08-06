use super::*;
#[cfg(target_os = "linux")]
use crate::platform::{KvmPlatform, Platform};

#[test]
fn alloc_256mib_returns_valid_pointer() {
    let mem = GuestMemory::new(256 * 1024 * 1024, 0).unwrap();
    assert!(!mem.host_addr().is_null());
    assert_eq!(mem.size(), 256 * 1024 * 1024);
}

#[test]
fn write_and_read_round_trip() {
    let mem = GuestMemory::new(4096, 0).unwrap();
    let data = b"hello visor";
    mem.write_bytes(0, data).unwrap();
    let read = mem.read_bytes(0, data.len()).unwrap();
    assert_eq!(read, data);
}

#[test]
fn guest_to_host_in_bounds() {
    let mem = GuestMemory::new(4096, 0).unwrap();
    assert!(mem.guest_to_host(0).is_some());
    assert!(mem.guest_to_host(4095).is_some());
}

#[test]
fn guest_to_host_out_of_bounds() {
    let mem = GuestMemory::new(4096, 0).unwrap();
    assert!(mem.guest_to_host(4096).is_none());
    assert!(mem.guest_to_host(u64::MAX).is_none());
}

#[test]
fn guest_to_host_with_nonzero_base() {
    let base = 0x1000_u64;
    let mem = GuestMemory::new(4096, base).unwrap();
    assert!(mem.guest_to_host(base).is_some());
    assert!(mem.guest_to_host(base + 4095).is_some());
    assert!(mem.guest_to_host(base + 4096).is_none());
    assert!(mem.guest_to_host(0).is_none()); // below base
}

#[test]
fn write_out_of_bounds_returns_error() {
    let mem = GuestMemory::new(4096, 0).unwrap();
    let result = mem.write_bytes(4096, b"x");
    assert!(result.is_err());
}

#[test]
fn write_overflow_returns_error() {
    let mem = GuestMemory::new(4096, 0).unwrap();
    let data = vec![0u8; 10];
    let result = mem.write_bytes(4090, &data);
    assert!(result.is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn register_with_kvm_succeeds() {
    let platform = KvmPlatform::new().unwrap();
    let vm = platform.create_vm().unwrap();
    let mem = GuestMemory::new(4096, 0).unwrap();
    mem.register(&vm, 0).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn register_and_write_read() {
    let platform = KvmPlatform::new().unwrap();
    let vm = platform.create_vm().unwrap();
    let mem = GuestMemory::new(64 * 1024, 0).unwrap();
    mem.register(&vm, 0).unwrap();

    // Write through guest memory, verify round-trip
    let payload = b"registered memory works";
    mem.write_bytes(0x100, payload).unwrap();
    let read = mem.read_bytes(0x100, payload.len()).unwrap();
    assert_eq!(read, payload);
}

#[test]
fn drop_unmaps_cleanly() {
    // Allocate and drop — verify no crash or leak
    let mem = GuestMemory::new(1024 * 1024, 0).unwrap();
    let _addr = mem.host_addr();
    drop(mem);
}

// -- HugePageSize enum -------------------------------------------------------

#[test]
fn huge_page_size_default_is_none() {
    assert!(matches!(HugePageSize::default(), HugePageSize::None));
}

#[test]
fn huge_page_size_bytes() {
    assert_eq!(HugePageSize::None.size_bytes(), 0);
    assert_eq!(HugePageSize::TwoMiB.size_bytes(), 2 * 1024 * 1024);
    assert_eq!(HugePageSize::OneGiB.size_bytes(), 1024 * 1024 * 1024);
}

#[test]
fn huge_page_size_display() {
    assert_eq!(format!("{}", HugePageSize::None), "none");
    assert_eq!(format!("{}", HugePageSize::TwoMiB), "2 MiB");
    assert_eq!(format!("{}", HugePageSize::OneGiB), "1 GiB");
}

// -- detect_huge_pages --------------------------------------------------------

#[cfg(target_os = "linux")]
#[test]
fn detect_huge_pages_returns_info() {
    // Should never error -- returns zero counts if hugepages not configured
    let info = detect_huge_pages().unwrap();
    // We don't assert specific counts since they depend on host config,
    // but the structure should be valid
    assert!(info.two_mib_available <= info.two_mib_total);
    assert!(info.one_gib_available <= info.one_gib_total);
}

#[cfg(target_os = "linux")]
#[test]
fn detect_huge_pages_from_custom_path_missing_dir() {
    // Non-existent path should return all zeros, not error
    let info = detect_huge_pages_from("/nonexistent/path/hugepages").unwrap();
    assert_eq!(info.two_mib_total, 0);
    assert_eq!(info.two_mib_available, 0);
    assert_eq!(info.one_gib_total, 0);
    assert_eq!(info.one_gib_available, 0);
}

#[cfg(target_os = "linux")]
#[test]
fn huge_page_info_has_2mib() {
    let info = HugePageInfo {
        two_mib_total: 10,
        two_mib_available: 5,
        one_gib_total: 0,
        one_gib_available: 0,
    };
    assert!(info.has(HugePageSize::TwoMiB));
    assert!(!info.has(HugePageSize::OneGiB));
    assert!(info.has(HugePageSize::None)); // None is always "available"
}

#[cfg(target_os = "linux")]
#[test]
fn huge_page_info_has_1gib() {
    let info = HugePageInfo {
        two_mib_total: 0,
        two_mib_available: 0,
        one_gib_total: 2,
        one_gib_available: 1,
    };
    assert!(!info.has(HugePageSize::TwoMiB));
    assert!(info.has(HugePageSize::OneGiB));
}

// -- with_huge_pages allocation + fallback ------------------------------------

#[cfg(target_os = "linux")]
#[test]
fn with_huge_pages_none_behaves_like_new() {
    let mem = GuestMemory::with_huge_pages(4096, 0, HugePageSize::None).unwrap();
    assert!(!mem.host_addr().is_null());
    assert_eq!(mem.size(), 4096);
    assert!(!mem.using_huge_pages());
}

#[cfg(target_os = "linux")]
#[test]
fn with_huge_pages_2mib_falls_back_when_unavailable() {
    // This system has 0 huge pages allocated, so mmap with MAP_HUGETLB
    // will fail and we should fall back to regular pages.
    let size = 2 * 1024 * 1024; // 2 MiB aligned
    let mem = GuestMemory::with_huge_pages(size, 0, HugePageSize::TwoMiB).unwrap();
    assert!(!mem.host_addr().is_null());
    assert_eq!(mem.size(), size);
    // Write/read still works regardless of huge page status
    let data = b"huge page test";
    mem.write_bytes(0, data).unwrap();
    let read = mem.read_bytes(0, data.len()).unwrap();
    assert_eq!(read, data);
}

#[cfg(target_os = "linux")]
#[test]
fn with_huge_pages_1gib_falls_back_when_unavailable() {
    let size = 1024 * 1024 * 1024; // 1 GiB aligned
    let mem = GuestMemory::with_huge_pages(size, 0, HugePageSize::OneGiB).unwrap();
    assert!(!mem.host_addr().is_null());
    assert_eq!(mem.size(), size);
}

#[cfg(target_os = "linux")]
#[test]
fn with_huge_pages_unaligned_size_falls_back() {
    // 3 MiB is not aligned to 2 MiB huge page boundary
    let size = 3 * 1024 * 1024;
    let mem = GuestMemory::with_huge_pages(size, 0, HugePageSize::TwoMiB).unwrap();
    assert!(!mem.host_addr().is_null());
    assert_eq!(mem.size(), size);
    // Unaligned size forces fallback to regular pages
    assert!(!mem.using_huge_pages());
}

#[cfg(target_os = "linux")]
#[test]
fn with_huge_pages_write_read_round_trip() {
    let size = 2 * 1024 * 1024;
    let mem = GuestMemory::with_huge_pages(size, 0, HugePageSize::TwoMiB).unwrap();
    let data = b"round trip through huge pages";
    mem.write_bytes(1024, data).unwrap();
    let read = mem.read_bytes(1024, data.len()).unwrap();
    assert_eq!(read, data);
}

#[cfg(target_os = "linux")]
#[test]
fn with_huge_pages_register_with_kvm() {
    let platform = KvmPlatform::new().unwrap();
    let vm = platform.create_vm().unwrap();
    let size = 2 * 1024 * 1024;
    let mem = GuestMemory::with_huge_pages(size, 0, HugePageSize::TwoMiB).unwrap();
    mem.register(&vm, 0).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn with_huge_pages_drop_unmaps_cleanly() {
    let size = 2 * 1024 * 1024;
    let mem = GuestMemory::with_huge_pages(size, 0, HugePageSize::TwoMiB).unwrap();
    let _addr = mem.host_addr();
    drop(mem);
}

#[test]
fn default_new_does_not_use_huge_pages() {
    let mem = GuestMemory::new(4096, 0).unwrap();
    assert!(!mem.using_huge_pages());
}

// -- from_shared_fd --------------------------------------------------------

#[test]
fn from_shared_fd_creates_valid_memory() {
    let name = "/visor-test-from-shared-fd";
    let region = crate::shared_memory::SharedMemoryRegion::create(name, 4096).unwrap();
    let mem = GuestMemory::from_shared_fd(region.fd(), 4096, 0x1000).unwrap();
    assert!(!mem.host_addr().is_null());
    assert_eq!(mem.size(), 4096);
    assert_eq!(mem.guest_base(), 0x1000);
    assert!(!mem.using_huge_pages());
    region.unlink().unwrap();
}

#[test]
fn from_shared_fd_write_visible_through_guest_memory() {
    let name = "/visor-test-shm-write-visible";
    let region = crate::shared_memory::SharedMemoryRegion::create(name, 4096).unwrap();

    // Write data through the SharedMemoryRegion pointer.
    let data = b"shared memory test";
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), region.as_ptr(), data.len());
    }

    // Map the same fd as GuestMemory and read through it.
    let mem = GuestMemory::from_shared_fd(region.fd(), 4096, 0).unwrap();
    let read = mem.read_bytes(0, data.len()).unwrap();
    assert_eq!(read, data);

    // Write through GuestMemory and verify via the original mapping.
    let data2 = b"reverse direction!";
    mem.write_bytes(0, data2).unwrap();
    let mut buf = vec![0u8; data2.len()];
    unsafe {
        std::ptr::copy_nonoverlapping(region.as_ptr(), buf.as_mut_ptr(), data2.len());
    }
    assert_eq!(buf, data2);

    region.unlink().unwrap();
}
