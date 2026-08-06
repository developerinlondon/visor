//! Tests for `aarch64` boot protocol setup.

use std::io::Write;
use std::path::PathBuf;

use super::*;
use crate::memory::GuestMemory;

// ── Test Helpers ─────────────────────────────────────────────────────────

/// Creates a minimal valid ARM64 Image with the given `text_offset`.
///
/// The header magic is set correctly at offset 0x38. The image body
/// (after the 64-byte header) is filled with `fill_byte`.
fn make_test_arm64_image(text_offset: u64, body_size: usize, fill_byte: u8) -> Vec<u8> {
    let total_size = body_size.max(ARM64_IMAGE_HEADER_SIZE);
    let mut image = vec![fill_byte; total_size];

    // code0: branch instruction (stub)
    image[0..4].copy_from_slice(&0x1400_0000u32.to_le_bytes());

    // text_offset at 0x08
    image[ARM64_IMAGE_TEXT_OFFSET_OFF..ARM64_IMAGE_TEXT_OFFSET_OFF + 8]
        .copy_from_slice(&text_offset.to_le_bytes());

    // flags at 0x30 (little-endian, 4K pages, position-independent)
    image[0x30..0x38].copy_from_slice(&0u64.to_le_bytes());

    // magic at 0x38
    image[ARM64_IMAGE_MAGIC_OFFSET..ARM64_IMAGE_MAGIC_OFFSET + 4]
        .copy_from_slice(&ARM64_IMAGE_MAGIC.to_le_bytes());

    image
}

/// Writes data to a temporary file and returns the handle and path.
fn write_to_tempfile(data: &[u8]) -> (tempfile::NamedTempFile, PathBuf) {
    let mut f = crate::testutil::named_temp_file("visor-vmm-boot-").unwrap();
    f.write_all(data).unwrap();
    f.flush().unwrap();
    let path = f.path().to_path_buf();
    (f, path)
}

/// Returns an [`FdtConfig`] with typical test values (256 MiB, 1 CPU).
fn test_fdt_config(cmdline: &str) -> FdtConfig<'_> {
    FdtConfig {
        memory_size: 256 * 1024 * 1024,
        num_cpus: 1,
        cmdline,
        gic_dist_addr: 0x0800_0000,
        gic_dist_size: 0x1_0000,
        gic_redist_addr: 0x080A_0000,
        gic_redist_size: 0xF6_0000,
        mmio_devices: &[],
    }
}

// ── Constants Tests ──────────────────────────────────────────────────────

#[test]
fn dram_start_is_at_2gib() {
    assert_eq!(DRAM_MEM_START, 0x8000_0000);
    assert_eq!(DRAM_MEM_START, 2 * 1024 * 1024 * 1024);
}

#[test]
fn kernel_load_offset_is_2mib() {
    assert_eq!(KERNEL_LOAD_OFFSET, 0x0020_0000);
    assert_eq!(KERNEL_LOAD_OFFSET, 2 * 1024 * 1024);
}

#[test]
fn fdt_max_size_is_2mib() {
    assert_eq!(FDT_MAX_SIZE, 0x0020_0000);
    assert_eq!(FDT_MAX_SIZE, 2 * 1024 * 1024);
}

#[test]
fn pstate_bits_correct() {
    // D=1, A=1, I=1, F=1 (bits 9..6) + EL1h mode (bits 3..0 = 0b0101)
    assert_eq!(PSTATE_FAULT_BITS_64, 0x3C5);
}

// ── FDT Address Tests ────────────────────────────────────────────────────

#[test]
fn fdt_addr_at_end_of_ram() {
    let addr = get_fdt_addr(256 * 1024 * 1024);
    assert!(addr >= DRAM_MEM_START, "FDT addr below DRAM start");
}

#[test]
fn fdt_addr_is_2mib_aligned() {
    for size_mib in [64, 128, 256, 512, 1024] {
        let size = size_mib * 1024 * 1024;
        let addr = get_fdt_addr(size);
        assert_eq!(
            addr % (2 * 1024 * 1024),
            0,
            "FDT addr {addr:#x} not 2 MiB aligned for {size_mib} MiB RAM"
        );
    }
}

#[test]
fn fdt_addr_leaves_room_for_fdt() {
    let memory_size: u64 = 256 * 1024 * 1024;
    let fdt_addr = get_fdt_addr(memory_size);
    let ram_end = DRAM_MEM_START + memory_size;
    assert!(
        fdt_addr + FDT_MAX_SIZE as u64 <= ram_end,
        "FDT at {fdt_addr:#x} + {:#x} exceeds RAM end {ram_end:#x}",
        FDT_MAX_SIZE
    );
}

#[test]
fn fdt_addr_different_for_different_sizes() {
    let addr_128 = get_fdt_addr(128 * 1024 * 1024);
    let addr_256 = get_fdt_addr(256 * 1024 * 1024);
    assert_ne!(
        addr_128, addr_256,
        "FDT addrs should differ for different RAM sizes"
    );
}

// ── ARM64 Image Loading Tests ────────────────────────────────────────────

#[test]
fn load_image_with_default_offset() {
    let image = make_test_arm64_image(0, 4096, 0xCC);
    let mem_size = 64 * 1024 * 1024;
    let memory = GuestMemory::new(mem_size, DRAM_MEM_START).unwrap();

    let entry = load_arm64_image(&memory, &image).unwrap();

    // text_offset=0 → default KERNEL_LOAD_OFFSET
    assert_eq!(entry, DRAM_MEM_START + KERNEL_LOAD_OFFSET);

    // Verify data was written
    let read_back = memory.read_bytes(entry, 4).unwrap();
    assert_eq!(read_back, &image[..4]);
}

#[test]
fn load_image_with_custom_text_offset() {
    let custom_offset: u64 = 0x0080_0000; // 8 MiB
    let image = make_test_arm64_image(custom_offset, 4096, 0xAA);
    let mem_size = 64 * 1024 * 1024;
    let memory = GuestMemory::new(mem_size, DRAM_MEM_START).unwrap();

    let entry = load_arm64_image(&memory, &image).unwrap();
    assert_eq!(entry, DRAM_MEM_START + custom_offset);
}

#[test]
fn load_image_writes_full_data() {
    let image = make_test_arm64_image(0, 8192, 0xBB);
    let mem_size = 64 * 1024 * 1024;
    let memory = GuestMemory::new(mem_size, DRAM_MEM_START).unwrap();

    let entry = load_arm64_image(&memory, &image).unwrap();

    // Verify entire image was written (spot-check header and tail)
    let header = memory.read_bytes(entry, ARM64_IMAGE_HEADER_SIZE).unwrap();
    assert_eq!(header, &image[..ARM64_IMAGE_HEADER_SIZE]);

    let tail = memory.read_bytes(entry + 8000, 192).unwrap();
    assert_eq!(tail, &image[8000..]);
}

#[test]
fn load_image_rejects_bad_magic() {
    let mut image = make_test_arm64_image(0, 4096, 0);
    // Corrupt the magic
    image[ARM64_IMAGE_MAGIC_OFFSET..ARM64_IMAGE_MAGIC_OFFSET + 4]
        .copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());

    let memory = GuestMemory::new(64 * 1024 * 1024, DRAM_MEM_START).unwrap();
    let result = load_arm64_image(&memory, &image);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("bad ARM64 Image magic"),
        "unexpected error: {err}"
    );
}

#[test]
fn load_image_rejects_truncated_header() {
    let memory = GuestMemory::new(64 * 1024 * 1024, DRAM_MEM_START).unwrap();
    let result = load_arm64_image(&memory, &[0u8; 32]);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("too small"),
        "unexpected error: {err}"
    );
}

#[test]
fn load_image_rejects_exceeding_memory() {
    let image = make_test_arm64_image(0, 8192, 0);
    // Guest memory: KERNEL_LOAD_OFFSET + 1 KiB — too small for 8 KiB image
    let mem_size = KERNEL_LOAD_OFFSET as usize + 1024;
    let memory = GuestMemory::new(mem_size, DRAM_MEM_START).unwrap();

    let result = load_arm64_image(&memory, &image);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("exceeds guest memory"),
        "unexpected error: {err}"
    );
}

#[test]
fn load_image_rejects_empty_data() {
    let memory = GuestMemory::new(64 * 1024 * 1024, DRAM_MEM_START).unwrap();
    let result = load_arm64_image(&memory, &[]);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("too small"),
        "unexpected error: {err}"
    );
}

// ── FDT Generation Tests ─────────────────────────────────────────────────

#[test]
fn create_fdt_produces_valid_blob() {
    let config = test_fdt_config("console=ttyAMA0");
    let blob = create_fdt(&config).unwrap();

    // FDT magic is 0xd00dfeed (big-endian) at offset 0
    assert!(blob.len() >= 4);
    let magic = u32::from_be_bytes([blob[0], blob[1], blob[2], blob[3]]);
    assert_eq!(magic, 0xd00d_feed, "bad FDT magic");
}

#[test]
fn create_fdt_with_multiple_cpus() {
    let mut config = test_fdt_config("");
    config.num_cpus = 4;
    let blob = create_fdt(&config).unwrap();

    let magic = u32::from_be_bytes([blob[0], blob[1], blob[2], blob[3]]);
    assert_eq!(magic, 0xd00d_feed);
}

#[test]
fn create_fdt_with_single_cpu() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    // "cpu@0" should appear in the blob
    let needle = b"cpu@0";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "cpu@0 not found in FDT");
}

#[test]
fn create_fdt_cmdline_embedded() {
    let cmdline = "console=ttyAMA0 reboot=k panic=1 root=/dev/vda rw";
    let config = test_fdt_config(cmdline);
    let blob = create_fdt(&config).unwrap();

    let cmdline_bytes = cmdline.as_bytes();
    let found = blob
        .windows(cmdline_bytes.len())
        .any(|w| w == cmdline_bytes);
    assert!(found, "cmdline not found in FDT blob");
}

#[test]
fn create_fdt_fits_within_max_size() {
    let config = test_fdt_config("console=ttyAMA0");
    let blob = create_fdt(&config).unwrap();
    assert!(
        blob.len() <= FDT_MAX_SIZE,
        "FDT blob {} bytes exceeds max {}",
        blob.len(),
        FDT_MAX_SIZE
    );
}

#[test]
fn create_fdt_contains_memory_node() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    let needle = b"memory@";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "memory node not found in FDT");
}

#[test]
fn create_fdt_contains_gic_node() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    let needle = b"arm,gic-v3";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "GIC node not found in FDT");
}

#[test]
fn create_fdt_contains_timer_node() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    let needle = b"arm,armv8-timer";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "timer node not found in FDT");
}

#[test]
fn create_fdt_contains_psci_node() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    let needle = b"arm,psci-0.2";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "PSCI node not found in FDT");
}

#[test]
fn create_fdt_psci_has_psci_1_0_compatible() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    // PSCI 1.0 should be the primary compatible string
    let needle = b"arm,psci-1.0";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "arm,psci-1.0 compatible not found in PSCI node");
}

#[test]
fn create_fdt_psci_has_hvc_method() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    let needle = b"hvc";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "hvc method not found in PSCI node");
}

#[test]
fn create_fdt_cpu_has_enable_method_psci() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    let needle = b"enable-method";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "enable-method property not found in CPU node");
}

#[test]
fn create_fdt_contains_chosen_node() {
    let config = test_fdt_config("test=1");
    let blob = create_fdt(&config).unwrap();

    let needle = b"chosen";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "chosen node not found in FDT");
}

#[test]
fn create_fdt_chosen_contains_stdout_path() {
    let config = test_fdt_config("console=ttyAMA0");
    let blob = create_fdt(&config).unwrap();

    // stdout-path should reference the PL011 UART node.
    let needle = b"stdout-path";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "stdout-path not found in chosen node");

    // The path should contain the UART base address.
    let uart_path = format!("pl011@{:x}", UART_BASE);
    let needle = uart_path.as_bytes();
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(
        found,
        "UART path '{uart_path}' not found in FDT stdout-path"
    );
}

#[test]
fn create_fdt_with_many_cpus() {
    let mut config = test_fdt_config("");
    config.num_cpus = 64;
    let blob = create_fdt(&config).unwrap();

    // Verify the last CPU node name is present
    let needle = b"cpu@63";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "cpu@63 not found in FDT for 64-CPU config");

    // Still fits within limits
    assert!(blob.len() <= FDT_MAX_SIZE);
}

// ── Virtio-MMIO FDT Node Tests ──────────────────────────────────────────

#[test]
fn create_fdt_with_no_mmio_devices() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    // No virtio_mmio nodes should be present
    let needle = b"virtio,mmio";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(!found, "virtio,mmio found in FDT with no MMIO devices");
}

#[test]
fn create_fdt_with_single_mmio_device() {
    let devices = [FdtMmioDevice::new(0xd000_0000, 0x1000, 5)];
    let mut config = test_fdt_config("");
    config.mmio_devices = &devices;
    let blob = create_fdt(&config).unwrap();

    // Check compatible string
    let needle = b"virtio,mmio";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "virtio,mmio not found in FDT");

    // Check node name
    let needle = b"virtio_mmio@d0000000";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "virtio_mmio@d0000000 not found in FDT");
}

#[test]
fn create_fdt_with_multiple_mmio_devices() {
    let devices = [
        FdtMmioDevice::new(0xd000_0000, 0x1000, 5),
        FdtMmioDevice::new(0xd000_1000, 0x1000, 6),
        FdtMmioDevice::new(0xd000_2000, 0x1000, 7),
    ];
    let mut config = test_fdt_config("");
    config.mmio_devices = &devices;
    let blob = create_fdt(&config).unwrap();

    // All three node names should appear
    for (addr, name) in [
        (0xd000_0000u64, "virtio_mmio@d0000000"),
        (0xd000_1000, "virtio_mmio@d0001000"),
        (0xd000_2000, "virtio_mmio@d0002000"),
    ] {
        let needle = name.as_bytes();
        let found = blob.windows(needle.len()).any(|w| w == needle);
        assert!(found, "{name} not found in FDT (addr={addr:#x})");
    }
}

#[test]
fn create_fdt_with_mmio_devices_contains_dma_coherent() {
    let devices = [FdtMmioDevice::new(0xd000_0000, 0x1000, 5)];
    let mut config = test_fdt_config("");
    config.mmio_devices = &devices;
    let blob = create_fdt(&config).unwrap();

    let needle = b"dma-coherent";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "dma-coherent property not found in FDT");
}

#[test]
fn create_fdt_with_mmio_devices_fits_within_max_size() {
    // Even with many devices, the FDT should fit
    let devices: Vec<FdtMmioDevice> = (0..16)
        .map(|i| FdtMmioDevice::new(0xd000_0000 + u64::from(i) * 0x1000, 0x1000, 5 + i))
        .collect();
    let mut config = test_fdt_config("");
    config.mmio_devices = &devices;
    let blob = create_fdt(&config).unwrap();

    assert!(
        blob.len() <= FDT_MAX_SIZE,
        "FDT blob {} bytes exceeds max {}",
        blob.len(),
        FDT_MAX_SIZE
    );
}

// ── PL011 UART FDT Node Tests ───────────────────────────────────────────

#[test]
fn create_fdt_contains_uart_node() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    let needle = b"pl011@9000000";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "pl011@9000000 node not found in FDT");
}

#[test]
fn create_fdt_uart_has_pl011_compatible() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    let needle = b"arm,pl011";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "arm,pl011 compatible not found in FDT");
}

#[test]
fn create_fdt_uart_has_primecell_compatible() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    let needle = b"arm,primecell";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "arm,primecell compatible not found in FDT");
}

#[test]
fn create_fdt_contains_clock_node() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    let needle = b"apb-pclk";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "apb-pclk clock node not found in FDT");
}

#[test]
fn create_fdt_clock_has_fixed_clock_compatible() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    let needle = b"fixed-clock";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "fixed-clock compatible not found in FDT");
}

#[test]
fn create_fdt_uart_has_clock_names() {
    let config = test_fdt_config("");
    let blob = create_fdt(&config).unwrap();

    // clock-names is a stringlist: "uartclk\0apb_pclk"
    let needle = b"uartclk";
    let found = blob.windows(needle.len()).any(|w| w == needle);
    assert!(found, "uartclk clock name not found in FDT");
}

#[test]
fn uart_constants_match_boot_layout() {
    assert_eq!(UART_BASE, 0x0900_0000);
    assert_eq!(UART_SIZE, 0x1000);
    assert_eq!(UART_GSI, 4);
}

// ── Integration: configure_boot() ────────────────────────────────────────

#[test]
fn configure_boot_returns_valid_config() {
    let image = make_test_arm64_image(0, 4096, 0x90);
    let (_tmp, path) = write_to_tempfile(&image);

    let mem_size: usize = 256 * 1024 * 1024;
    let memory = GuestMemory::new(mem_size, DRAM_MEM_START).unwrap();
    let fdt_config = test_fdt_config("console=ttyAMA0");

    let config = configure_boot(&memory, &path, &fdt_config).unwrap();

    assert_eq!(config.entry_point, DRAM_MEM_START + KERNEL_LOAD_OFFSET);
    assert!(config.fdt_addr >= DRAM_MEM_START);
    assert_eq!(
        config.fdt_addr % (2 * 1024 * 1024),
        0,
        "FDT addr not 2 MiB aligned"
    );
}

#[test]
fn configure_boot_writes_fdt_to_memory() {
    let image = make_test_arm64_image(0, 4096, 0x90);
    let (_tmp, path) = write_to_tempfile(&image);

    let mem_size: usize = 256 * 1024 * 1024;
    let memory = GuestMemory::new(mem_size, DRAM_MEM_START).unwrap();
    let fdt_config = test_fdt_config("console=ttyAMA0");

    let config = configure_boot(&memory, &path, &fdt_config).unwrap();

    // FDT magic should be at fdt_addr in guest memory
    let fdt_header = memory.read_bytes(config.fdt_addr, 4).unwrap();
    let magic = u32::from_be_bytes([fdt_header[0], fdt_header[1], fdt_header[2], fdt_header[3]]);
    assert_eq!(magic, 0xd00d_feed, "FDT not written to guest memory");
}

#[test]
fn configure_boot_writes_kernel_to_memory() {
    let image = make_test_arm64_image(0, 4096, 0xCC);
    let (_tmp, path) = write_to_tempfile(&image);

    let mem_size: usize = 256 * 1024 * 1024;
    let memory = GuestMemory::new(mem_size, DRAM_MEM_START).unwrap();
    let fdt_config = test_fdt_config("");

    let config = configure_boot(&memory, &path, &fdt_config).unwrap();

    // Verify kernel data at entry point
    let kernel_data = memory.read_bytes(config.entry_point, 4).unwrap();
    assert_eq!(kernel_data, &image[..4]);
}

#[test]
fn configure_boot_kernel_and_fdt_do_not_overlap() {
    let image = make_test_arm64_image(0, 4096, 0x90);
    let (_tmp, path) = write_to_tempfile(&image);

    let mem_size: usize = 256 * 1024 * 1024;
    let memory = GuestMemory::new(mem_size, DRAM_MEM_START).unwrap();
    let fdt_config = test_fdt_config("console=ttyAMA0");

    let config = configure_boot(&memory, &path, &fdt_config).unwrap();

    let kernel_end = config.entry_point + image.len() as u64;
    assert!(
        kernel_end <= config.fdt_addr,
        "kernel [{:#x}..{kernel_end:#x}] overlaps FDT at {:#x}",
        config.entry_point,
        config.fdt_addr
    );
}

#[test]
fn configure_boot_fails_with_nonexistent_kernel() {
    let mem_size: usize = 64 * 1024 * 1024;
    let memory = GuestMemory::new(mem_size, DRAM_MEM_START).unwrap();
    let fdt_config = test_fdt_config("");

    let result = configure_boot(&memory, &PathBuf::from("/nonexistent/kernel"), &fdt_config);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("failed to read kernel"),
        "unexpected error: {err}"
    );
}
