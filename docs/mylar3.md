# Mylar3

Comics library manager. Monitors and organizes your comic library.

- **Port**: 8090
- **Image**: `lscr.io/linuxserver/mylar3:latest`
- **Compose**: [compose.yml](../compose.yml)
- **Upstream**: <https://github.com/mylar3/mylar3>

## Notes

- Comics are stored on the volume mounted into the container (see `compose.yml`) — point it at wherever your comic library lives.
- The web UI is served on port `8090` by default.

## Port conflict note

If another service on the host already listens on `8090`, mylar3 will fail to
bind. Map it to a free host port instead — e.g. `-p 8091:8090/tcp` (or the
equivalent `ports:` entry in `compose.yml`).
