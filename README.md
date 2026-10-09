<p align="center">
  <img src="assets/icon-256.png" width="120" alt="mylar3" />
</p>

# mylar3

Mylar3 is a comic book (CBR/CBZ) library manager and organizer.

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
    lscr.io/linuxserver/mylar3:latest
```

**LXC** — on a container-capable LXC (e.g. a Proxmox LXC with nesting enabled) run the same image via Docker/Podman as above, or install mylar3 from upstream directly on the guest: <https://github.com/MylarComics/mylar3>.

**VM** — install mylar3 from upstream (<https://github.com/MylarComics/mylar3>) or run the same container image inside the VM; expose port `8090`.

**Unraid** — add via *Community Applications*, or *Docker → Add Container* with image `lscr.io/linuxserver/mylar3:latest`, port `8090`, and the volume paths above.

### Ports & data

| | |
|---|---|
| Default port | `8090` |
| Upstream | <https://github.com/MylarComics/mylar3> |
| Operator notes | [mylar3.md](docs/mylar3.md) |


### Backup & restore

Back up the config/data volume(s) above — that's the whole service state (stop the container first for a clean copy). Restore by putting them back and starting it.

> With orca this is **`service.backup` / `service.restore`** — location-agnostic (docker / podman / lxc / vm), one command regardless of where mylar3 runs. No per-service backup script.

## With orca

orca drives this plugin through the generic `service.*` surface:

```sh
orca service.deploy mylar3      # render + launch on any supported runtime
orca service.status mylar3      # health + rich diagnostics (typed payload)
orca service.backup mylar3      # location-agnostic backup (tar; PBS on Proxmox)
orca service.configure mylar3   # refuses; settings go through mylar3.configure
```

plus detect + remediate tools against a registered endpoint (`mylar3.create`):

```sh
orca mylar3.status --name comics                      # findings: retention, post-processing, stuck grabs, torrents
orca mylar3.configure --name comics                   # dry run: reports settings drift
orca mylar3.configure --name comics --usenet-retention 6000 --execute
orca mylar3.backlog.process --name comics --folder /downloads/completed/comics --execute
```

Mylar has no per-setting write: `mylar3.configure --execute` submits the web
settings form with every checkbox and newznab/torznab provider re-posted at its
current value. It refuses when this Mylar's form does not match the one it
knows (missing checkboxes or keys v0.11.0 does not define, `minimal_ini` not
False), fails if the settings changed since the plan, and fails, listing what
moved, if anything besides the planned changes moved on write. Without `--usenet-retention` it only
raises retention below 6000. `backlog.process` only accepts a folder inside
`sab_directory` or `check_folder` and clear of the library.

`status`, `configure` and `backlog.process` all read Mylar's settings from the
web UI's `/getConfig`, so when the web UI uses basic auth the endpoint needs
`web_username`/`web_password`, and forms login is not supported: `configure` and
`backlog.process` refuse, and `status` reports the settings as unknown.

## Layout

- `src/` — the plugin (pure Rust): the `ServiceBackend` descriptor + the `mylar3.` detect/remediate tools.
- `docs/` — standalone operator notes.
- [CAPABILITIES.md](CAPABILITIES.md) — the service-backend contract checklist.
- `assets/` — plugin icon.
