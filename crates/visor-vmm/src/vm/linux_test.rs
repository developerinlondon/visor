use super::*;

#[test]
fn fs_mmio_location_starts_at_expected_base_and_irq() {
    let (base, irq) = fs_mmio_location(0, 0, 0).expect("fs slot should be allocated");

    assert_eq!(base, 0xd000_3000);
    assert_eq!(irq, 8);
}

#[test]
fn fs_mmio_location_advances_one_slot_per_shared_dir() {
    let (base, irq) = fs_mmio_location(2, 0, 0).expect("fs slot should be allocated");

    assert_eq!(base, 0xd000_5000);
    assert_eq!(irq, 10);
}

#[test]
fn fs_mmio_location_rejects_indices_that_do_not_fit_in_u32() {
    let too_many = usize::try_from(u32::MAX).expect("u32::MAX fits in usize") + 1;

    assert!(matches!(
        fs_mmio_location(too_many, 0, 0),
        Err(VmBootError::Device(message)) if message == "too many shared dirs"
    ));
}

#[test]
fn build_acpi_mmio_devices_adds_one_entry_per_shared_dir() {
    let mmio_devices =
        build_acpi_mmio_devices(2, 0, 0).expect("mmio layout should be built for shared dirs");

    assert_eq!(mmio_devices.len(), 5);
    assert_eq!(mmio_devices[3].base_addr, 0xd000_3000);
    assert_eq!(mmio_devices[3].size, 0x1000);
    assert_eq!(mmio_devices[3].gsi, 8);
    assert_eq!(mmio_devices[4].base_addr, 0xd000_4000);
    assert_eq!(mmio_devices[4].size, 0x1000);
    assert_eq!(mmio_devices[4].gsi, 9);
}

#[test]
fn fs_mmio_location_shifts_when_networking_is_enabled() {
    let (base, irq) = fs_mmio_location(0, 0, 1).expect("fs slot should be allocated");

    assert_eq!(base, 0xd000_4000);
    assert_eq!(irq, 9);
}

#[test]
fn build_acpi_mmio_devices_reserves_mmio_slot_for_net_device() {
    let mmio_devices =
        build_acpi_mmio_devices(2, 0, 1).expect("mmio layout should be built for networking");

    assert_eq!(mmio_devices.len(), 6);
    assert_eq!(mmio_devices[3].base_addr, 0xd000_3000);
    assert_eq!(mmio_devices[3].size, 0x1000);
    assert_eq!(mmio_devices[3].gsi, 8);
    assert_eq!(mmio_devices[4].base_addr, 0xd000_4000);
    assert_eq!(mmio_devices[4].size, 0x1000);
    assert_eq!(mmio_devices[4].gsi, 9);
    assert_eq!(mmio_devices[5].base_addr, 0xd000_5000);
    assert_eq!(mmio_devices[5].size, 0x1000);
    assert_eq!(mmio_devices[5].gsi, 10);
}

#[test]
fn extra_block_mmio_location_starts_after_rootfs_slot() {
    let (base, irq) = extra_block_mmio_location(0).expect("data disk slot should be allocated");

    assert_eq!(base, 0xd000_1000);
    assert_eq!(irq, 6);
}

#[test]
fn build_acpi_mmio_devices_includes_extra_block_devices() {
    let mmio_devices = build_acpi_mmio_devices(1, 2, 1)
        .expect("mmio layout should be built for data disks and networking");

    assert_eq!(mmio_devices.len(), 7);
    assert_eq!(mmio_devices[1].base_addr, 0xd000_1000);
    assert_eq!(mmio_devices[1].gsi, 6);
    assert_eq!(mmio_devices[2].base_addr, 0xd000_2000);
    assert_eq!(mmio_devices[2].gsi, 7);
    assert_eq!(mmio_devices[3].base_addr, 0xd000_3000);
    assert_eq!(mmio_devices[3].gsi, 8);
    assert_eq!(mmio_devices[4].base_addr, 0xd000_4000);
    assert_eq!(mmio_devices[4].gsi, 9);
    assert_eq!(mmio_devices[6].base_addr, 0xd000_6000);
    assert_eq!(mmio_devices[6].gsi, 11);
}
