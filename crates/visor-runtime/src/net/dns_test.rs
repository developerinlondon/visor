use std::net::Ipv4Addr;

use super::*;

// ── DnsRegistry ──────────────────────────────────────────────────────

#[test]
fn empty_registry_returns_none() {
    let registry = DnsRegistry::new();
    assert!(registry.resolve("web").is_none());
}

#[test]
fn register_and_resolve_vm_name() {
    let mut registry = DnsRegistry::new();
    registry.register("web", Ipv4Addr::new(172, 20, 0, 2));
    let ip = registry.resolve("web");
    assert_eq!(ip, Some(Ipv4Addr::new(172, 20, 0, 2)));
}

#[test]
fn register_multiple_names() {
    let mut registry = DnsRegistry::new();
    registry.register("web", Ipv4Addr::new(172, 20, 0, 2));
    registry.register("db", Ipv4Addr::new(172, 20, 0, 3));
    registry.register("redis", Ipv4Addr::new(172, 20, 0, 4));

    assert_eq!(registry.resolve("web"), Some(Ipv4Addr::new(172, 20, 0, 2)));
    assert_eq!(registry.resolve("db"), Some(Ipv4Addr::new(172, 20, 0, 3)));
    assert_eq!(
        registry.resolve("redis"),
        Some(Ipv4Addr::new(172, 20, 0, 4))
    );
}

#[test]
fn unregister_removes_name() {
    let mut registry = DnsRegistry::new();
    registry.register("web", Ipv4Addr::new(172, 20, 0, 2));
    assert!(registry.resolve("web").is_some());

    registry.unregister("web");
    assert!(registry.resolve("web").is_none());
}

#[test]
fn unregister_nonexistent_is_ok() {
    let mut registry = DnsRegistry::new();
    registry.unregister("nonexistent"); // should not panic
}

#[test]
fn resolve_is_case_insensitive() {
    let mut registry = DnsRegistry::new();
    registry.register("Web-Server", Ipv4Addr::new(172, 20, 0, 2));
    assert_eq!(
        registry.resolve("web-server"),
        Some(Ipv4Addr::new(172, 20, 0, 2))
    );
    assert_eq!(
        registry.resolve("WEB-SERVER"),
        Some(Ipv4Addr::new(172, 20, 0, 2))
    );
}

#[test]
fn register_overwrites_existing() {
    let mut registry = DnsRegistry::new();
    registry.register("web", Ipv4Addr::new(172, 20, 0, 2));
    registry.register("web", Ipv4Addr::new(172, 20, 0, 5));
    assert_eq!(registry.resolve("web"), Some(Ipv4Addr::new(172, 20, 0, 5)));
}

#[test]
fn count_tracks_entries() {
    let mut registry = DnsRegistry::new();
    assert_eq!(registry.count(), 0);

    registry.register("web", Ipv4Addr::new(172, 20, 0, 2));
    assert_eq!(registry.count(), 1);

    registry.register("db", Ipv4Addr::new(172, 20, 0, 3));
    assert_eq!(registry.count(), 2);

    registry.unregister("web");
    assert_eq!(registry.count(), 1);
}

#[test]
fn all_entries_returns_snapshot() {
    let mut registry = DnsRegistry::new();
    registry.register("web", Ipv4Addr::new(172, 20, 0, 2));
    registry.register("db", Ipv4Addr::new(172, 20, 0, 3));

    let entries = registry.all_entries();
    assert_eq!(entries.len(), 2);
}

// ── DnsResolverConfig ────────────────────────────────────────────────

#[test]
fn resolver_config_defaults() {
    let config = DnsResolverConfig::new(Ipv4Addr::new(172, 20, 0, 1));
    assert_eq!(config.listen_ip(), Ipv4Addr::new(172, 20, 0, 1));
    assert_eq!(config.listen_port(), 53);
    assert!(!config.upstream_servers().is_empty());
}

#[test]
fn resolver_config_with_custom_port() {
    let config = DnsResolverConfig::new(Ipv4Addr::new(172, 20, 0, 1)).with_port(5353);
    assert_eq!(config.listen_port(), 5353);
}

#[test]
fn resolver_config_with_upstream() {
    let config = DnsResolverConfig::new(Ipv4Addr::new(172, 20, 0, 1))
        .with_upstream(Ipv4Addr::new(1, 1, 1, 1));
    assert!(
        config
            .upstream_servers()
            .contains(&Ipv4Addr::new(1, 1, 1, 1))
    );
}

// ── Reverse lookup ───────────────────────────────────────────────────

#[test]
fn reverse_lookup_returns_name() {
    let mut registry = DnsRegistry::new();
    registry.register("web", Ipv4Addr::new(172, 20, 0, 2));

    let name = registry.reverse_lookup(Ipv4Addr::new(172, 20, 0, 2));
    assert_eq!(name, Some("web"));
}

#[test]
fn reverse_lookup_unknown_returns_none() {
    let registry = DnsRegistry::new();
    assert!(
        registry
            .reverse_lookup(Ipv4Addr::new(172, 20, 0, 99))
            .is_none()
    );
}
