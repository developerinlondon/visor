//! Docker-style random name generator for VMs.
//!
//! Generates short names like `swift_flash`, `noble_storm` when users
//! don't provide an explicit `--name`.

/// Generates a random `adjective_superhero` name.
#[must_use]
pub fn generate_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seed = dur
        .as_secs()
        .wrapping_mul(1_000_000_000)
        .wrapping_add(u64::from(dur.subsec_nanos()));
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let hash = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(n.wrapping_mul(1_442_695_040_888_963_407));

    let adj_idx = usize::try_from(hash % ADJECTIVES.len() as u64).unwrap_or(0);
    let hero_idx = usize::try_from((hash >> 16) % HEROES.len() as u64).unwrap_or(0);
    let adj = ADJECTIVES[adj_idx];
    let hero = HEROES[hero_idx];
    format!("{adj}_{hero}")
}

const ADJECTIVES: &[&str] = &[
    "agile", "bold", "brisk", "calm", "clever", "cool", "eager", "fiery", "fresh", "keen", "kind",
    "lucky", "merry", "mighty", "noble", "quick", "ready", "sharp", "solid", "spry", "swift",
    "tidy", "vivid", "witty", "zesty",
];

const HEROES: &[&str] = &[
    "atom", "batman", "batgirl", "beast", "bishop", "blade", "blink", "cyclops", "falcon", "flash",
    "frozone", "gamora", "groot", "hawkeye", "hulk", "jubilee", "rocket", "robin", "rogue",
    "shuri", "spawn", "static", "storm", "vision", "wasp",
];

#[cfg(test)]
#[path = "names_test.rs"]
mod tests;
