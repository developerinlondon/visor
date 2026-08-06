# Beta Compatibility Contract

Updated: 2026-03-09

This document defines the Docker and Compose behavior Visor currently treats as
part of the Linux beta contract.

It is intentionally narrower than "Docker-compatible" in the abstract. If a
workflow is not listed here as supported, it should be treated as partial or
out of scope until we explicitly promote it.

Current execution note:

- the focused Docker and Compose coverage listed below is green on a clean
  Linux host
- the e2e harness now tears down backend VMs, network rules, and muxer sockets
  so one failing run does not poison the next one

## Scope

```text
+------------------- Beta Promise --------------------+
| Linux host | OCI workloads | Docker CLI core flows |
| Compose default-project networking | shell/exec     |
+--------------------------+--------------------------+
                           |
                           v
+------------------- Not A Promise Yet ---------------+
| Full Docker Engine parity | Full Docker network     |
| arbitrary bind semantics  | production multi-tenant |
| non-Linux host support    | generic VM platform     |
+-----------------------------------------------------+
```

## Docker Support Boundary

### Supported and Test-Backed

| Area                             | Status    | Notes                                                |
| -------------------------------- | --------- | ---------------------------------------------------- |
| `docker run`                     | Supported | Core container start path is covered                 |
| `docker run -d`                  | Supported | Detached lifecycle is covered                        |
| `docker run -p HOST:CONTAINER`   | Supported | Localhost reachability is covered                    |
| `docker pull`                    | Supported | Pulled images can be run after import                |
| `docker exec`                    | Supported | Non-interactive exec is covered                      |
| `docker exec -i`                 | Supported | stdin-attached non-TTY exec is covered               |
| `docker exec -it`                | Supported | PTY exec path is covered                             |
| `docker logs`                    | Supported | Container stdout retrieval is covered                |
| `docker stop`                    | Supported | Graceful stop path is covered                        |
| `docker rm`                      | Supported | Removal path is covered                              |
| `docker build`                   | Supported | Classic Docker build path is covered                 |
| `docker buildx build --load`     | Supported | Built images are imported back into Visor            |
| image inspect after build/import | Supported | Built and loaded images are inspectable and runnable |

### Implemented but Not a Beta Parity Promise

| Area                                             | Current state | Why not promised yet                                                        |
| ------------------------------------------------ | ------------- | --------------------------------------------------------------------------- |
| Docker HTTP API breadth outside the tested flows | Partial       | The shim exposes more endpoints than the beta contract guarantees           |
| `events`                                         | Present       | Useful for compatibility, but not yet part of the beta promise              |
| volume metadata endpoints                        | Present       | We do not yet claim full Docker volume parity                               |
| network metadata endpoints                       | Present       | We do not yet claim full Docker network parity                              |
| `docker kill`                                    | Implemented   | Not currently part of the explicitly test-backed beta contract              |
| archive upload endpoints                         | Implemented   | Used by compatibility flows, but not yet documented as a broad user promise |

### Known Docker Limits

| Limit                                                           | Current behavior                                                                                               |
| --------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| Host bind address specificity                                   | Beta promise is host-port publication on the local host; explicit host-IP binding semantics are not guaranteed |
| Full Docker Engine parity                                       | Not promised                                                                                                   |
| Swarm / overlay / daemon-cluster features                       | Not promised                                                                                                   |
| Broad API-version compatibility claims beyond the tested subset | Not promised                                                                                                   |

## Compose and Networking Beta Contract

### Supported and Test-Backed

| Area                                           | Status    | Notes                                                  |
| ---------------------------------------------- | --------- | ------------------------------------------------------ |
| `docker compose up -d`                         | Supported | Multi-service startup is covered                       |
| `docker compose ps`                            | Supported | Project-scoped container listing is covered            |
| `docker compose logs`                          | Supported | Service log retrieval is covered                       |
| `docker compose stop`                          | Supported | Project-scoped service stop is covered                 |
| `docker compose start`                         | Supported | Project-scoped service restart is covered              |
| `docker compose exec -T`                       | Supported | Non-TTY project-scoped exec is covered                 |
| `docker compose down -v`                       | Supported | Project teardown is covered                            |
| two concurrent Compose projects                | Supported | Concurrent project isolation is covered                |
| same-project bare service-name resolution      | Supported | Bare service aliases are covered                       |
| same-project qualified service-name resolution | Supported | `<service>.<project>` aliases are covered              |
| cross-project service-name isolation           | Supported | Cross-project qualified aliases stay isolated          |
| published host-port reachability               | Supported | Localhost access to published service ports is covered |

### Beta Network Contract

For the Linux beta, Visor currently promises:

- default per-project Compose isolation
- service discovery within a project by service name and
  `<service>.<project>`
- localhost reachability for published TCP ports
- independent concurrent Compose projects on the same Linux host

### Known Compose and Network Limits

| Limit                                        | Current behavior                                                                                                         |
| -------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| Full Docker network parity                   | Not promised                                                                                                             |
| Arbitrary custom network-driver behavior     | Not promised                                                                                                             |
| Non-default Compose network semantics        | Declared Compose networks map to real guest attachments on Linux; arbitrary custom driver behavior is still not promised |
| Host-IP-specific publish semantics           | Not part of the current beta contract                                                                                    |
| UDP parity                                   | Not part of the current beta contract                                                                                    |
| Broad Compose lifecycle beyond covered flows | `up -d`, `ps`, `logs`, `stop`, `start`, `exec -T`, and `down -v` are the explicit beta promise today                     |

## Validation Baseline

The current beta contract is grounded in these focused checks:

- `cargo test -p visor-docker --quiet`
- `cargo test -p visor-vmm muxer_drop_removes_listener_socket_path --quiet`
- `cargo test --test e2e_docker docker_smoke_matrix_covers_run_exec_logs_stop_rm_and_build -- --exact --quiet`
- `cargo test --test e2e_docker docker_compose_projects_are_isolated_and_reachable -- --exact --quiet`
- `cargo test --test e2e_docker docker_compose_multi_network_scopes_service_resolution -- --exact --quiet`
- `cargo test --test e2e e2e_nested_builder_vm_reaches_alpine_mirrors_and_runs_qemu_img -- --exact --quiet`
- `cargo test --test e2e_docker docker_compose_lifecycle_covers_logs_stop_and_start -- --exact --quiet`
- `cargo test --test e2e_docker docker_buildx_load_imports_image_into_visor -- --exact --quiet`

That test file currently covers:

- Docker core lifecycle
- stdin and TTY exec variants
- published-port reachability
- Compose project isolation and service discovery
- `buildx --load` import and run

## Non-Goals for This Beta Cut

- full Docker Engine parity
- full Docker network-driver parity
- a generic VM or libvirt-compatible platform
- non-Linux host support
