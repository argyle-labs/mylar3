<p align="center">
  <img src="assets/icon-256.png" width="120" alt="mylar3" />
</p>

# mylar3

Mylar3 is an automated comic book (CBR/CBZ) downloader and manager.

A first-party [orca](https://github.com/argyle-labs/orca) plugin (service-backend).

This repo is **self-contained** — the steps below run mylar3 **by hand, without orca**. orca automates exactly this (same image, ports, and data) through one generic surface.

---

## Run it without orca

### Docker Compose

```yaml
# compose.yml
services:
  mylar3:
    image: lscr.io/linuxserver/mylar3:latest
    container_name: mylar3
    restart: unless-stopped
    ports:
      - "8090:8090/tcp"   # web UI
    volumes:
      - ./config:/config
      - /path/to/comics:/comics
      - /path/to/downloads:/downloads
```

```sh
docker compose up -d
```

### Other runtimes

**Podman** — the compose above works with `podman compose up -d`, or run it directly:

```sh
podman run -d --name mylar3 --restart unless-stopped \
    -p 8090:8090/tcp \
    -v ./config:/config \
    -v /path/to/comics:/comics \
    -v /path/to/downloads:/downloads \
    lscr.io/linuxserver/mylar3:latest
```

**LXC** — on a container-capable LXC (e.g. a Proxmox LXC with nesting enabled) run the same image via Docker/Podman as above, or install mylar3 from upstream directly on the guest: <https://github.com/mylar3/mylar3>.

**VM** — install mylar3 from upstream (<https://github.com/mylar3/mylar3>) or run the same container image inside the VM; expose port `8090`.

**Unraid** — add via *Community Applications*, or *Docker → Add Container* with image `lscr.io/linuxserver/mylar3:latest`, port `8090`, and the volume paths above.

### Ports & data

| | |
|---|---|
| Default port | `8090` |
| Upstream | <https://github.com/mylar3/mylar3> |
| Operator notes | [mylar3.md](docs/mylar3.md) |


### Backup & restore

Back up the config/data volume(s) above — that's the whole service state (stop the container first for a clean copy). Restore by putting them back and starting it.

> With orca this is **`service.backup` / `service.restore`** — location-agnostic (docker / podman / lxc / vm), one command regardless of where mylar3 runs. No per-service backup script.

## With orca

orca drives this plugin through the single generic `service.*` surface — no per-plugin tools:

```sh
orca service.deploy mylar3      # render + launch on any supported runtime
orca service.status mylar3      # health + rich diagnostics (typed payload)
orca service.backup mylar3      # location-agnostic backup (tar; PBS on Proxmox)
orca service.configure mylar3   # apply config via the upstream API
```

## Layout

- `src/` — the plugin (pure Rust): the `ServiceBackend` descriptor + `configure` / `status`.
- `docs/` — standalone operator notes.
- [CAPABILITIES.md](CAPABILITIES.md) — the service-backend contract checklist.
- `assets/` — plugin icon.
