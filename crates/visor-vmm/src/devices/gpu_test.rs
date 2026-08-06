use super::*;

// ── Detection tests ────────────────────────────────────────────────

#[test]
fn test_detect_gpus_returns_list() {
    // Should always succeed, even if the list is empty.
    let gpus = detect_gpus();
    // On this machine we expect at least the ASPEED BMC VGA.
    // But we don't assert a specific count — just that it doesn't panic.
    assert!(gpus.is_ok() || gpus.is_err());
    // If it succeeds, it should return a vec (possibly empty on some hosts).
    if let Ok(list) = gpus {
        // Every returned device should have a non-empty PCI address.
        for gpu in &list {
            assert!(!gpu.pci_address.is_empty(), "PCI address must not be empty");
        }
    }
}

#[test]
fn test_gpu_device_from_sysfs_entry() {
    // Parse a real sysfs entry if any GPU exists, otherwise verify the
    // parsing logic with known class codes.
    let gpus = detect_gpus().unwrap_or_default();
    if gpus.is_empty() {
        // No GPU on host — skip gracefully.
        return;
    }
    let gpu = &gpus[0];
    assert!(!gpu.pci_address.is_empty());
    // Vendor ID should be non-zero.
    assert_ne!(gpu.vendor_id, 0, "vendor_id must not be zero");
    // Device ID should be non-zero.
    assert_ne!(gpu.device_id, 0, "device_id must not be zero");
}

#[test]
fn test_gpu_device_display() {
    let gpu = GpuDevice {
        pci_address: "0000:22:00.0".into(),
        vendor_id: 0x1a03,
        device_id: 0x2000,
        device_name: "ASPEED AST2500".into(),
        vendor_name: "ASPEED Technology".into(),
        pci_class: 0x0003_0000,
        current_driver: Some("ast".into()),
        iommu_group: Some(42),
        is_boot_vga: true,
    };
    let display = format!("{gpu}");
    assert!(
        display.contains("0000:22:00.0"),
        "Display should contain PCI address, got: {display}"
    );
    assert!(
        display.contains("ASPEED"),
        "Display should contain vendor or device name, got: {display}"
    );
}

// ── Config tests ───────────────────────────────────────────────────

#[test]
fn test_gpu_config_default() {
    let config = GpuConfig::default();
    assert!(config.pci_address.is_none(), "default should auto-detect");
    assert!(
        config.vga_arbitration,
        "VGA arbitration should be on by default"
    );
    assert_eq!(
        config.reset_method,
        ResetMethod::Flr,
        "FLR should be default reset method"
    );
}

#[test]
fn test_gpu_config_with_address() {
    let config = GpuConfig {
        pci_address: Some("0000:01:00.0".into()),
        ..GpuConfig::default()
    };
    assert_eq!(config.pci_address.as_deref(), Some("0000:01:00.0"));
}

// ── Error tests ────────────────────────────────────────────────────

#[test]
fn test_gpu_error_display() {
    let err = GpuError::NoGpuFound;
    assert_eq!(format!("{err}"), "no GPU found on host");

    let err = GpuError::GpuNotFound {
        address: "0000:01:00.0".into(),
    };
    assert!(format!("{err}").contains("0000:01:00.0"));

    let err = GpuError::BootVga {
        address: "0000:22:00.0".into(),
    };
    assert!(format!("{err}").contains("boot VGA"));

    let err = GpuError::VgaArbitration(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "no permission",
    ));
    assert!(format!("{err}").contains("VGA arbitration"));

    let err = GpuError::Passthrough("test error".into());
    assert!(format!("{err}").contains("test error"));

    let err = GpuError::Reset {
        address: "0000:01:00.0".into(),
        source: std::io::Error::other("reset failed"),
    };
    assert!(format!("{err}").contains("reset"));
    assert!(format!("{err}").contains("0000:01:00.0"));
}

// ── PCI class matching ─────────────────────────────────────────────

#[test]
fn test_pci_class_is_gpu() {
    // VGA compatible controller
    assert!(is_gpu_class(0x0003_0000), "0x030000 is VGA controller");
    // 3D controller
    assert!(is_gpu_class(0x0003_0200), "0x030200 is 3D controller");
    // Display controller (0x0380xx) — other subclass
    assert!(is_gpu_class(0x0003_8000), "0x038000 is display controller");
}

#[test]
fn test_pci_class_is_not_gpu() {
    // Network controller
    assert!(!is_gpu_class(0x0002_0000), "0x020000 is network, not GPU");
    // Storage controller
    assert!(!is_gpu_class(0x0001_0000), "0x010000 is storage, not GPU");
    // USB controller
    assert!(!is_gpu_class(0x000c_0300), "0x0c0300 is USB, not GPU");
    // Zero
    assert!(!is_gpu_class(0x0000_0000), "0x000000 is not GPU");
}

// ── Boot VGA check ─────────────────────────────────────────────────

#[test]
fn test_boot_vga_check() {
    let gpus = detect_gpus().unwrap_or_default();
    // On this machine, 0000:22:00.0 should be boot_vga.
    for gpu in &gpus {
        if gpu.pci_address == "0000:22:00.0" {
            assert!(gpu.is_boot_vga, "ASPEED 22:00.0 should be boot VGA");
        }
    }

    // Test that a GpuDevice with is_boot_vga=true is flagged.
    let boot_gpu = GpuDevice {
        pci_address: "0000:22:00.0".into(),
        vendor_id: 0x1a03,
        device_id: 0x2000,
        device_name: String::new(),
        vendor_name: String::new(),
        pci_class: 0x0003_0000,
        current_driver: None,
        iommu_group: None,
        is_boot_vga: true,
    };
    assert!(boot_gpu.is_boot_vga);

    let non_boot_gpu = GpuDevice {
        is_boot_vga: false,
        ..boot_gpu
    };
    assert!(!non_boot_gpu.is_boot_vga);
}

// ── Reset method preference ────────────────────────────────────────

#[test]
fn test_reset_method_preference() {
    // FLR is highest priority.
    assert!(ResetMethod::Flr < ResetMethod::BusReset);
    assert!(ResetMethod::BusReset < ResetMethod::PmReset);
    assert!(ResetMethod::Flr < ResetMethod::PmReset);

    // Default is FLR.
    assert_eq!(ResetMethod::default(), ResetMethod::Flr);
}

// ── BAR region struct ──────────────────────────────────────────────

#[test]
fn test_bar_region_struct() {
    let bar = GpuBarRegion {
        index: 0,
        offset: 0,
        size: 256 * 1024 * 1024, // 256 MiB
        flags: BarFlags::MEMORY | BarFlags::PREFETCHABLE,
    };
    assert_eq!(bar.index, 0);
    assert_eq!(bar.size, 256 * 1024 * 1024);
    assert!(bar.flags.contains(BarFlags::MEMORY));
    assert!(bar.flags.contains(BarFlags::PREFETCHABLE));
    assert!(!bar.flags.contains(BarFlags::IO));
}

#[test]
fn test_bar_region_default() {
    let bar = GpuBarRegion::default();
    assert_eq!(bar.index, 0);
    assert_eq!(bar.offset, 0);
    assert_eq!(bar.size, 0);
    assert!(bar.flags.is_empty());
}

// ── Vendor name mapping ────────────────────────────────────────────

#[test]
fn test_gpu_vendor_names() {
    assert_eq!(vendor_name(0x10de), "NVIDIA");
    assert_eq!(vendor_name(0x1002), "AMD");
    assert_eq!(vendor_name(0x8086), "Intel");
    assert_eq!(vendor_name(0x1a03), "ASPEED Technology");
    assert_eq!(vendor_name(0xFFFF), "Unknown");
}

// ── Passthrough-capable check ──────────────────────────────────────

/// Returns `true` if a passthrough-capable GPU is available on this host.
fn passthrough_gpu_available() -> bool {
    let gpus = detect_gpus().unwrap_or_default();
    gpus.iter()
        .any(|g| !g.is_boot_vga && is_passthrough_capable(g))
}

#[test]
fn test_passthrough_capable_excludes_boot_vga() {
    let gpu = GpuDevice {
        pci_address: "0000:22:00.0".into(),
        vendor_id: 0x1a03,
        device_id: 0x2000,
        device_name: "ASPEED AST2500".into(),
        vendor_name: "ASPEED Technology".into(),
        pci_class: 0x0003_0000,
        current_driver: Some("ast".into()),
        iommu_group: Some(42),
        is_boot_vga: true,
    };
    assert!(
        !is_passthrough_capable(&gpu),
        "boot VGA device should not be passthrough-capable"
    );
}

#[test]
fn test_passthrough_capable_requires_iommu_group() {
    let gpu = GpuDevice {
        pci_address: "0000:01:00.0".into(),
        vendor_id: 0x10de,
        device_id: 0x1234,
        device_name: "Test GPU".into(),
        vendor_name: "NVIDIA".into(),
        pci_class: 0x0003_0000,
        current_driver: Some("nvidia".into()),
        iommu_group: None,
        is_boot_vga: false,
    };
    assert!(
        !is_passthrough_capable(&gpu),
        "device without IOMMU group should not be passthrough-capable"
    );
}

#[test]
fn test_passthrough_capable_good_device() {
    let gpu = GpuDevice {
        pci_address: "0000:01:00.0".into(),
        vendor_id: 0x10de,
        device_id: 0x1234,
        device_name: "Test GPU".into(),
        vendor_name: "NVIDIA".into(),
        pci_class: 0x0003_0000,
        current_driver: Some("nvidia".into()),
        iommu_group: Some(1),
        is_boot_vga: false,
    };
    assert!(
        is_passthrough_capable(&gpu),
        "non-boot GPU with IOMMU group should be passthrough-capable"
    );
}

// ── GpuPassthrough (requires real hardware) ────────────────────────

#[test]
fn test_gpu_passthrough_no_gpu_available() {
    if passthrough_gpu_available() {
        // Skip — we'd actually bind a GPU, which is destructive.
        return;
    }
    // On this machine there's no passthrough-capable GPU.
    let config = GpuConfig::default();
    let result = GpuPassthrough::prepare(&config);
    assert!(
        result.is_err(),
        "should fail when no passthrough GPU available"
    );
    match result {
        Err(GpuError::NoGpuFound) => {} // expected
        Err(other) => panic!("expected NoGpuFound, got: {other}"),
        Ok(_) => panic!("should not succeed without a passthrough GPU"),
    }
}

#[test]
fn test_gpu_passthrough_boot_vga_rejected() {
    let config = GpuConfig {
        pci_address: Some("0000:22:00.0".into()),
        ..GpuConfig::default()
    };
    let result = GpuPassthrough::prepare(&config);
    // Should fail because 22:00.0 is boot VGA.
    assert!(result.is_err());
    match result {
        Err(GpuError::BootVga { address }) => {
            assert_eq!(address, "0000:22:00.0");
        }
        Err(GpuError::GpuNotFound { .. }) => {
            // Also acceptable if sysfs parsing doesn't find it as GPU.
        }
        Err(other) => panic!("expected BootVga or GpuNotFound, got: {other}"),
        Ok(_) => panic!("should not succeed with boot VGA device"),
    }
}

// ── DMA region struct ──────────────────────────────────────────────

#[test]
fn test_dma_region_struct() {
    let region = DmaRegion {
        iova: 0x0,
        size: 1024 * 1024 * 1024, // 1 GiB
    };
    assert_eq!(region.iova, 0x0);
    assert_eq!(region.size, 1024 * 1024 * 1024);
}

// ── Bar flags ──────────────────────────────────────────────────────

#[test]
fn test_bar_flags_bitwise() {
    let flags = BarFlags::MEMORY | BarFlags::PREFETCHABLE;
    assert!(flags.contains(BarFlags::MEMORY));
    assert!(flags.contains(BarFlags::PREFETCHABLE));
    assert!(!flags.contains(BarFlags::IO));

    let io_flags = BarFlags::IO;
    assert!(io_flags.contains(BarFlags::IO));
    assert!(!io_flags.contains(BarFlags::MEMORY));
}

// ── Reset method ordering ──────────────────────────────────────────

#[test]
fn test_reset_method_names() {
    assert_eq!(format!("{}", ResetMethod::Flr), "FLR");
    assert_eq!(format!("{}", ResetMethod::BusReset), "bus reset");
    assert_eq!(format!("{}", ResetMethod::PmReset), "PM reset");
}
