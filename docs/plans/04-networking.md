# 04 — Networking

## Shared Network Model

Unlike livecontainers (one TAP + NAT per VM), visor uses shared networks with
an internal virtual switch. VMs on the same network communicate directly via
in-process memory copies — no host kernel network stack involved.

```
LIVECONTAINERS (old) — one TAP + NAT per VM:
  VM-1 ── TAP-1 ── iptables ── host
  VM-2 ── TAP-2 ── iptables ── host     ← 3 TAPs, 9+ iptables rules
  VM-3 ── TAP-3 ── iptables ── host

VISOR (new) — shared networks, internal switching:
  Default network "visor0" (172.20.0.0/24):
    VM-1 (172.20.0.2) ──┐
    VM-2 (172.20.0.3) ──┼── internal vswitch ── one TAP ── NAT ── internet
    VM-3 (172.20.0.4) ──┘

  Compose network "myapp" (172.21.0.0/24):
    web   (172.21.0.2) ──┐
    db    (172.21.0.3) ──┼── internal vswitch ── one TAP ── NAT ── internet
    redis (172.21.0.4) ──┘
```

Benefits:

- **Fewer host resources** — one TAP + NAT per network, not per VM
- **Faster inter-VM** — memory copy between virtqueues (~10μs vs ~100μs via TAP)
- **Service discovery** — embedded DNS resolves VM names within a network
- **Isolation** — VMs on different networks can't see each other
- **Docker-compatible** — same mental model as Docker bridge networks

## Internal Virtual Switch

Since all VMs are threads in one process, the vswitch is a Rust struct that
routes packets between VM virtio-net backends in shared memory:

```
VM-1 virtio-net TX queue
    → vswitch reads packet
    → checks destination MAC
    → writes to VM-2 virtio-net RX queue
    → signals VM-2 (eventfd)

No syscalls. No kernel. Just memory copies within the process.
```

For external traffic (internet-bound), the vswitch forwards packets to a TAP
device connected to the host network via NAT.

```rust
struct VirtualSwitch {
    name: String,
    subnet: Ipv4Net,              // e.g., 172.20.0.0/24
    gateway: Ipv4Addr,            // e.g., 172.20.0.1 (daemon's DNS lives here)
    ports: HashMap<MacAddr, VmNetPort>,  // connected VMs
    tap: Option<TapDevice>,       // external gateway (NAT to host)
}

struct VmNetPort {
    vm_id: String,
    ip: Ipv4Addr,
    mac: MacAddr,
    tx_queue: VirtioNetQueue,     // VM sends packets here
    rx_queue: VirtioNetQueue,     // vswitch delivers packets here
    rx_eventfd: EventFd,          // wake VM when packet arrives
}
```

## Embedded DNS Resolver

Each network's gateway IP (e.g., 172.20.0.1) runs a DNS resolver inside the
daemon. Guests have `nameserver 172.20.0.1` in `/etc/resolv.conf`.

```
VM "web" does: getaddrinfo("db")
  → DNS query to 172.20.0.1:53
  → daemon's DNS resolver
  → "db" is on network "myapp" at 172.21.0.3
  → returns A record 172.21.0.3

VM "web" does: getaddrinfo("google.com")
  → DNS query to 172.20.0.1:53
  → daemon doesn't know "google.com"
  → forwards upstream (host's /etc/resolv.conf)
  → returns A record
```

This gives service discovery by VM name within a network — essential for compose.
No external DNS server needed.

Implementation: lightweight UDP listener on the gateway IP, parsing DNS wire
format. For internal names, return the VM's IP. For external names, forward to
upstream resolvers. Use `trust-dns-resolver` or `hickory-dns` crate.

## Port Forwarding

Map a host port to a guest port:

```bash
visor run -p 8080:80 nginx:alpine
```

Daemon sets up iptables DNAT: `host:8080 → guest_ip:80`. Multiple port mappings
supported. Works across all networks.

```
External client → host:8080 → iptables DNAT → TAP → vswitch → VM virtio-net → guest:80
```

## Network Lifecycle

```bash
# Default network created automatically on `visor start`
visor start
  → creates network "visor0" (172.20.0.0/24)

# VMs join default network unless specified
visor run alpine echo hello
  → VM gets 172.20.0.2 on "visor0"

# Compose creates isolated networks per project
visor compose up -f myapp.yml
  → creates network "myapp_default" (172.21.0.0/24)
  → web gets 172.21.0.2, db gets 172.21.0.3
  → web can reach db by name "db"

# Multiple composes get separate networks
visor compose up -f another.yml
  → creates network "another_default" (172.22.0.0/24)
  → isolated from myapp_default

# Manual network management
visor network create mynet --subnet 172.30.0.0/24
visor run --network mynet alpine sh
visor network ls
visor network rm mynet
```

## Guest Networking Setup

visor-init configures guest networking using raw ioctls (no iproute2 needed):

1. `SIOCSIFADDR` — set IP address on eth0
2. `SIOCSIFNETMASK` — set subnet mask
3. `SIOCSIFFLAGS` — bring interface UP
4. `RTM_NEWROUTE` (netlink) — add default gateway route
5. Write `/etc/resolv.conf` — `nameserver <gateway_ip>`
6. Set hostname

Guest MAC address: deterministic from network + VM index.
Guest IP: assigned by daemon's IP allocator for that network.
Config passed via `/lc/run.json` on the init drive.

## VM Access

| Method          | How                                     | Use Case                    |
| --------------- | --------------------------------------- | --------------------------- |
| `visor exec`    | daemon → vsock → visor-init → fork/exec | Primary. Like `docker exec` |
| Port forwarding | `-p 2222:22` → iptables DNAT            | Network services            |
| Direct IP       | VM IP reachable from host via TAP       | Advanced / debugging        |

## TLS

TLS is handled at different layers:

| Layer           | Responsibility                                      | Who                      |
| --------------- | --------------------------------------------------- | ------------------------ |
| Daemon API      | Unix socket (default, no TLS) or TCP+TLS for remote | visor daemon             |
| mTLS            | `--tls-ca ca.pem` for client cert auth              | visor daemon             |
| Service TLS     | nginx/app inside VM needs HTTPS                     | User's responsibility    |
| TLS termination | Route by hostname, terminate TLS                    | External (Traefik/Caddy) |
| Ingress         | Full reverse proxy, path routing, certs             | External (Traefik/Caddy) |

Daemon API TLS options:

```bash
# Local only (default) — unix socket, no TLS needed
visor start

# Remote access with TLS
visor start --listen 0.0.0.0:8443 --tls-cert cert.pem --tls-key key.pem

# mTLS (production / multi-tenant)
visor start --listen 0.0.0.0:8443 --tls-cert cert.pem --tls-key key.pem --tls-ca ca.pem
```

## Ingress

Outside visor's scope. Users run Traefik/Caddy/nginx on the host, pointing at
visor's forwarded ports. This is the Docker model.

P2 nice-to-have: simple hostname-based routing. `visor run --hostname myapp.local nginx`
and visor routes Host-header traffic. But full ingress (TLS termination, path
routing, rate limiting, automatic certs) stays external.
