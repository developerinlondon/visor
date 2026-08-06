use super::*;

// ── PCI address parsing ─────────────────────────────────────────────

#[test]
fn pci_address_parse_valid() {
    let addr: VfioPciAddress = "0000:03:00.0".parse().unwrap();
    assert_eq!(addr.domain(), 0);
    assert_eq!(addr.bus(), 3);
    assert_eq!(addr.device(), 0);
    assert_eq!(addr.function(), 0);
}

#[test]
fn pci_address_parse_nonzero_domain() {
    let addr: VfioPciAddress = "0001:0a:1f.7".parse().unwrap();
    assert_eq!(addr.domain(), 1);
    assert_eq!(addr.bus(), 0x0A);
    assert_eq!(addr.device(), 0x1F);
    assert_eq!(addr.function(), 7);
}

#[test]
fn pci_address_parse_max_values() {
    let addr: VfioPciAddress = "ffff:ff:1f.7".parse().unwrap();
    assert_eq!(addr.domain(), 0xFFFF);
    assert_eq!(addr.bus(), 0xFF);
    assert_eq!(addr.device(), 31);
    assert_eq!(addr.function(), 7);
}

#[test]
fn pci_address_parse_invalid_format() {
    assert!("invalid".parse::<VfioPciAddress>().is_err());
    assert!("00:00.0".parse::<VfioPciAddress>().is_err());
    assert!("0000:00:00".parse::<VfioPciAddress>().is_err());
    assert!("0000:00:00.".parse::<VfioPciAddress>().is_err());
    assert!("".parse::<VfioPciAddress>().is_err());
}

#[test]
fn pci_address_parse_device_out_of_range() {
    // Device > 31 is invalid (5-bit field)
    assert!("0000:00:20.0".parse::<VfioPciAddress>().is_err());
}

#[test]
fn pci_address_parse_function_out_of_range() {
    // Function > 7 is invalid (3-bit field)
    assert!("0000:00:00.8".parse::<VfioPciAddress>().is_err());
}

#[test]
fn pci_address_display_roundtrip() {
    let addr = VfioPciAddress::new(0, 3, 0, 0);
    let s = addr.to_string();
    assert_eq!(s, "0000:03:00.0");

    let parsed: VfioPciAddress = s.parse().unwrap();
    assert_eq!(parsed, addr);
}

#[test]
fn pci_address_display_complex() {
    let addr = VfioPciAddress::new(0x0001, 0xFF, 0x1F, 7);
    assert_eq!(addr.to_string(), "0001:ff:1f.7");
}

#[test]
fn pci_address_components() {
    let addr = VfioPciAddress::new(0x1234, 0xAB, 0x0C, 3);
    assert_eq!(addr.domain(), 0x1234);
    assert_eq!(addr.bus(), 0xAB);
    assert_eq!(addr.device(), 0x0C);
    assert_eq!(addr.function(), 3);
}

#[test]
fn pci_address_sysfs_path() {
    let addr = VfioPciAddress::new(0, 3, 0, 0);
    assert_eq!(
        addr.sysfs_path().to_str().unwrap(),
        "/sys/bus/pci/devices/0000:03:00.0"
    );
}

#[test]
fn pci_address_equality() {
    let a = VfioPciAddress::new(0, 3, 0, 0);
    let b: VfioPciAddress = "0000:03:00.0".parse().unwrap();
    assert_eq!(a, b);

    let c = VfioPciAddress::new(0, 4, 0, 0);
    assert_ne!(a, c);
}

// ── VFIO container tests ────────────────────────────────────────────

#[test]
fn container_open_succeeds() {
    if !std::path::Path::new("/dev/vfio/vfio").exists() {
        return; // Skip on hosts without VFIO
    }
    let container = VfioContainer::open();
    assert!(
        container.is_ok(),
        "failed to open VFIO container: {container:?}"
    );
}

#[test]
fn container_api_version() {
    if !std::path::Path::new("/dev/vfio/vfio").exists() {
        return;
    }
    let container = VfioContainer::open().unwrap();
    // API version should be 0 per VFIO spec
    let version = container.api_version().unwrap();
    assert_eq!(version, 0, "unexpected VFIO API version");
}

#[test]
fn container_check_extension_iommu_type1() {
    if !std::path::Path::new("/dev/vfio/vfio").exists() {
        return;
    }
    let container = VfioContainer::open().unwrap();
    let supported = container.check_extension(VFIO_TYPE1_IOMMU).unwrap();
    assert!(supported, "IOMMU type 1 should be supported on this host");
}

#[test]
fn container_debug_format() {
    if !std::path::Path::new("/dev/vfio/vfio").exists() {
        return;
    }
    let container = VfioContainer::open().unwrap();
    let debug = format!("{container:?}");
    assert!(debug.contains("VfioContainer"));
    assert!(debug.contains("fd"));
}

// ── VFIO group tests ────────────────────────────────────────────────

#[test]
fn group_open_nonexistent_fails() {
    // Group 999999 almost certainly doesn't exist
    let result = VfioGroup::open(999_999);
    assert!(result.is_err());
}

// ── VFIO error display ──────────────────────────────────────────────

#[test]
fn vfio_error_display_messages() {
    let err = VfioError::ApiVersion {
        expected: 0,
        actual: 99,
    };
    let msg = err.to_string();
    assert!(msg.contains("version mismatch"), "got: {msg}");
    assert!(msg.contains("99"), "got: {msg}");

    let err = VfioError::GroupNotViable { group_id: 42 };
    let msg = err.to_string();
    assert!(msg.contains("42"), "got: {msg}");
    assert!(msg.contains("not viable"), "got: {msg}");

    let err = VfioError::ParseAddress {
        input: "bad".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("bad"), "got: {msg}");
}

#[test]
fn find_iommu_group_nonexistent_device() {
    let addr = VfioPciAddress::new(0xFF, 0xFF, 0x1F, 7);
    let result = VfioPciAddress::find_iommu_group(&addr);
    assert!(result.is_err());
}

#[test]
fn current_driver_nonexistent_device() {
    let addr = VfioPciAddress::new(0xFF, 0xFF, 0x1F, 7);
    let result = VfioPciAddress::current_driver(&addr);
    // Should return Ok(None) or Err — either is acceptable for nonexistent device
    match result {
        Ok(None) | Err(_) => {} // Either is acceptable for nonexistent device
        Ok(Some(name)) => panic!("unexpected driver {name} for nonexistent device"),
    }
}

// ── DMA mapping struct ──────────────────────────────────────────────

#[test]
fn dma_map_new_rw() {
    let map = VfioDmaMap::new_rw(0x1000, 0x2000, 0x3000);
    assert_eq!(map.iova, 0x1000);
    assert_eq!(map.size, 0x2000);
    assert_eq!(map.user_addr, 0x3000);
    assert_eq!(map.flags, VFIO_DMA_MAP_FLAG_READ | VFIO_DMA_MAP_FLAG_WRITE);
}

// ── Region info struct ──────────────────────────────────────────────

#[test]
fn region_info_new() {
    let info = VfioRegionInfo::new(0, 0x1_0000, 0, 0x07);
    assert_eq!(info.index, 0);
    assert_eq!(info.size, 0x1_0000);
    assert_eq!(info.offset, 0);
    assert!(info.is_readable());
    assert!(info.is_writable());
    assert!(info.is_mmapable());
}

#[test]
fn region_info_flags() {
    // Read-only
    let ro = VfioRegionInfo::new(0, 0x1000, 0, 0x01);
    assert!(ro.is_readable());
    assert!(!ro.is_writable());
    assert!(!ro.is_mmapable());

    // No flags
    let none = VfioRegionInfo::new(0, 0x1000, 0, 0x00);
    assert!(!none.is_readable());
    assert!(!none.is_writable());
    assert!(!none.is_mmapable());
}

// ── ioctl constant verification ─────────────────────────────────────

#[test]
fn ioctl_constants_match_expected_values() {
    // Verify our calculated ioctl numbers match what the kernel expects.
    // _IO(';', N) = (0x3B << 8) | N
    assert_eq!(VFIO_GET_API_VERSION, (0x3B << 8) | 100);
    assert_eq!(VFIO_CHECK_EXTENSION, (0x3B << 8) | 101);
    assert_eq!(VFIO_SET_IOMMU, (0x3B << 8) | 102);
    assert_eq!(VFIO_GROUP_GET_STATUS, (0x3B << 8) | 103);
    assert_eq!(VFIO_GROUP_SET_CONTAINER, (0x3B << 8) | 104);
    assert_eq!(VFIO_GROUP_GET_DEVICE_FD, (0x3B << 8) | 106);
    assert_eq!(VFIO_DEVICE_GET_INFO, (0x3B << 8) | 107);
    assert_eq!(VFIO_DEVICE_GET_REGION_INFO, (0x3B << 8) | 108);
    assert_eq!(VFIO_DEVICE_SET_IRQS, (0x3B << 8) | 110);
    assert_eq!(VFIO_DEVICE_RESET, (0x3B << 8) | 111);
    assert_eq!(VFIO_IOMMU_MAP_DMA, (0x3B << 8) | 113);
    assert_eq!(VFIO_IOMMU_UNMAP_DMA, (0x3B << 8) | 114);
}

#[test]
fn vfio_constants_are_correct() {
    assert_eq!(VFIO_API_VERSION, 0);
    assert_eq!(VFIO_TYPE1_IOMMU, 1);
    assert_eq!(VFIO_GROUP_FLAGS_VIABLE, 1);
    assert_eq!(VFIO_DMA_MAP_FLAG_READ, 1);
    assert_eq!(VFIO_DMA_MAP_FLAG_WRITE, 2);
}

// ── Container drop closes fd ────────────────────────────────────────

#[test]
fn container_drop_closes_fd() {
    if !std::path::Path::new("/dev/vfio/vfio").exists() {
        return;
    }
    let raw_fd;
    {
        let container = VfioContainer::open().unwrap();
        raw_fd = container.as_raw_fd();
        // Container drops here
    }
    // After drop, the fd should be closed. We can verify by checking
    // that fcntl on it fails with EBADF.
    let ret = unsafe {
        // SAFETY: Testing that a closed fd returns EBADF. This is read-only
        // and does not affect any open file descriptors.
        libc::fcntl(raw_fd, libc::F_GETFD)
    };
    assert_eq!(ret, -1, "fd should be closed after drop");
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::EBADF),
        "should get EBADF for closed fd"
    );
}
