# Mylar3

Comics manager. Monitors and downloads comic issues, integrates with Prowlarr for indexer searching.

- **Host**: <host> (<ip>)
- **Port**: 8090
- **Image**: `lscr.io/linuxserver/mylar3`
- **Compose**: [compose/mylar3/docker-compose.yml](../../compose/mylar3/docker-compose.yml)
- **Network**: `media`

## Notes

- Prowlarr integration: add Mylar3 as an app in Prowlarr → Settings → Apps
- Comics stored at `/mnt/<host>/data/media/comics`
- Kapowarr also runs on <host> — Kapowarr focuses on ComicVine/manga; Mylar3 has stronger Prowlarr integration for western comics. Run both and use whichever fits each title better.
- Readarr (books) was retired June 2025 — LazyLibrarian covers books/ebooks instead

## Port conflict note

Default mylar3 port (8090) conflicts with qbittorrent (8090). Set `MYLAR3_PORT` to a different port (e.g. `8091`) in Portainer stack env vars.
