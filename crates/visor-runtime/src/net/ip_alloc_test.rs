use std::net::Ipv4Addr;

use super::*;

// ── Construction ──────────────────────────────────────────────────────

#[test]
fn new_allocator_has_correct_subnet() {
    let alloc = SubnetAllocator::new(
        Ipv4Addr::new(172, 20, 0, 0),
        24,
        Ipv4Addr::new(172, 20, 0, 1),
    )
    .unwrap();
    assert_eq!(alloc.base(), Ipv4Addr::new(172, 20, 0, 0));
    assert_eq!(alloc.prefix(), 24);
    assert_eq!(alloc.gateway(), Ipv4Addr::new(172, 20, 0, 1));
}

#[test]
fn default_allocator_uses_visor0_defaults() {
    let alloc = SubnetAllocator::default_network().unwrap();
    assert_eq!(alloc.base(), Ipv4Addr::new(172, 20, 0, 0));
    assert_eq!(alloc.prefix(), 24);
    assert_eq!(alloc.gateway(), Ipv4Addr::new(172, 20, 0, 1));
}

// ── Allocation ────────────────────────────────────────────────────────

#[test]
fn first_allocation_skips_network_and_gateway() {
    let alloc = SubnetAllocator::default_network().unwrap();
    let ip = alloc.allocate().unwrap();
    // .0 is network, .1 is gateway, first usable is .2
    assert_eq!(ip, Ipv4Addr::new(172, 20, 0, 2));
}

#[test]
fn sequential_allocations_are_unique() {
    let alloc = SubnetAllocator::default_network().unwrap();
    let ip1 = alloc.allocate().unwrap();
    let ip2 = alloc.allocate().unwrap();
    let ip3 = alloc.allocate().unwrap();
    assert_ne!(ip1, ip2);
    assert_ne!(ip2, ip3);
    assert_ne!(ip1, ip3);
    assert_eq!(ip1, Ipv4Addr::new(172, 20, 0, 2));
    assert_eq!(ip2, Ipv4Addr::new(172, 20, 0, 3));
    assert_eq!(ip3, Ipv4Addr::new(172, 20, 0, 4));
}

#[test]
fn cannot_allocate_broadcast_address() {
    // Fill up the subnet (2..254 = 253 addresses)
    let alloc = SubnetAllocator::default_network().unwrap();
    for _ in 0..253 {
        alloc.allocate().unwrap();
    }
    // Next allocation should fail — .255 is broadcast
    let result = alloc.allocate();
    assert!(result.is_err(), "should not allocate broadcast address");
}

// ── Release ──────────────────────────────────────────────────────────

#[test]
fn release_makes_ip_available_again() {
    let alloc = SubnetAllocator::default_network().unwrap();
    let ip = alloc.allocate().unwrap();
    assert_eq!(ip, Ipv4Addr::new(172, 20, 0, 2));

    alloc.release(ip).unwrap();
    let ip2 = alloc.allocate().unwrap();
    assert_eq!(ip2, Ipv4Addr::new(172, 20, 0, 2));
}

#[test]
fn release_network_address_fails() {
    let alloc = SubnetAllocator::default_network().unwrap();
    let result = alloc.release(Ipv4Addr::new(172, 20, 0, 0));
    assert!(result.is_err(), "should not release network address");
}

#[test]
fn release_gateway_address_fails() {
    let alloc = SubnetAllocator::default_network().unwrap();
    let result = alloc.release(Ipv4Addr::new(172, 20, 0, 1));
    assert!(result.is_err(), "should not release gateway address");
}

#[test]
fn release_broadcast_address_fails() {
    let alloc = SubnetAllocator::default_network().unwrap();
    let result = alloc.release(Ipv4Addr::new(172, 20, 0, 255));
    assert!(result.is_err(), "should not release broadcast address");
}

#[test]
fn release_unallocated_ip_fails() {
    let alloc = SubnetAllocator::default_network().unwrap();
    let result = alloc.release(Ipv4Addr::new(172, 20, 0, 50));
    assert!(result.is_err(), "should not release unallocated IP");
}

#[test]
fn release_out_of_subnet_fails() {
    let alloc = SubnetAllocator::default_network().unwrap();
    let result = alloc.release(Ipv4Addr::new(10, 0, 0, 1));
    assert!(result.is_err(), "should not release IP outside subnet");
}

// ── Thread Safety ─────────────────────────────────────────────────────

#[test]
fn allocator_is_thread_safe() {
    use std::sync::Arc;
    use std::thread;

    let alloc = Arc::new(SubnetAllocator::default_network().unwrap());
    let mut handles = vec![];

    for _ in 0..10 {
        let alloc = Arc::clone(&alloc);
        handles.push(thread::spawn(move || alloc.allocate().unwrap()));
    }

    let mut ips: Vec<Ipv4Addr> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    ips.sort();
    ips.dedup();
    assert_eq!(ips.len(), 10, "all 10 allocations should be unique");
}

// ── Count ────────────────────────────────────────────────────────────

#[test]
fn allocated_count_tracks_usage() {
    let alloc = SubnetAllocator::default_network().unwrap();
    assert_eq!(alloc.allocated_count(), 0);

    let ip = alloc.allocate().unwrap();
    assert_eq!(alloc.allocated_count(), 1);

    alloc.allocate().unwrap();
    assert_eq!(alloc.allocated_count(), 2);

    alloc.release(ip).unwrap();
    assert_eq!(alloc.allocated_count(), 1);
}

#[test]
fn available_count_is_correct() {
    let alloc = SubnetAllocator::default_network().unwrap();
    // /24 = 256 addresses, minus .0 (network), .1 (gateway), .255 (broadcast) = 253
    assert_eq!(alloc.available_count(), 253);

    alloc.allocate().unwrap();
    assert_eq!(alloc.available_count(), 252);
}

// ── Contains ─────────────────────────────────────────────────────────

#[test]
fn contains_ip_checks_subnet_membership() {
    let alloc = SubnetAllocator::default_network().unwrap();
    assert!(alloc.contains(Ipv4Addr::new(172, 20, 0, 100)));
    assert!(!alloc.contains(Ipv4Addr::new(10, 0, 0, 1)));
    assert!(!alloc.contains(Ipv4Addr::new(172, 21, 0, 1)));
}
