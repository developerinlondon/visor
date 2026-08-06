//! Docker Compose file parser and orchestrator.
//!
//! Parses `docker-compose.yml` files into an internal [`ComposeProject`]
//! representation and orchestrates multi-service deployments.
//! Supports services, networks, volumes, `depends_on`,
//! environment variables, ports, and image fields.
//!
//! # Example
//!
//! ```rust,no_run
//! use visor_runtime::compose::{parse_compose, ComposeProject};
//!
//! let yaml = r#"
//! services:
//!   web:
//!     image: nginx:latest
//!     ports:
//!       - "8080:80"
//! "#;
//!
//! let project: ComposeProject = parse_compose(yaml).unwrap();
//! ```

pub mod orchestrator;
pub mod parser;
pub mod types;

pub use orchestrator::{
    ComposeInstance, Orchestrator, ServiceStatus, dependency_sort, needs_health_wait,
};
pub use parser::{parse_compose, parse_compose_file, parse_compose_with_vars};
pub use types::{
    ComposeDependsOn, ComposeEnvironment, ComposeIpam, ComposeIpamConfig, ComposeNetwork,
    ComposePort, ComposeProject, ComposeService, ComposeVolumeConfig, DependsOnCondition,
};
