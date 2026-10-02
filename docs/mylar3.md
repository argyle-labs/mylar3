# Mylar3

Comics library manager. Monitors and organizes your comic library.

- **Port**: 8090
- **Image**: `lscr.io/linuxserver/mylar3:latest`
- **Compose**: [compose.yml](../compose.yml)
- **Upstream**: <https://github.com/mylar3/mylar3>

## Notes

- Comics are stored on the volume mounted into the container (see `compose.yml`) — point it at wherever your comic library lives.
- The web UI is served on port `8090` by default.

## Getting it actually working (operational notes)

A fresh Mylar3 will not find or download anything until these are set up:

### 1. ComicVine API key is mandatory

Mylar3 uses ComicVine for all comic metadata and series matching. Without a key
it cannot identify series or search. Create a free key at
<https://comicvine.gamespot.com/api/> and set it under
**Settings → Web Interface → ComicVine API**. Expect ComicVine rate limits on
large initial imports — Mylar3 throttles automatically.

### 2. Indexers via Prowlarr

Register Mylar3 in **Prowlarr → Settings → Apps** (implementation *Mylar*, full
sync) so it inherits your indexers, then confirm the **Books/Comics (7030)** and
relevant torrent/usenet categories are enabled. Attach a download client
(SABnzbd and/or qBittorrent) under **Settings → Download Settings**.

### 3. Library + post-processing paths

- **Comic Location**: point at your comics library tree (the same directory your
  reader indexes, e.g. `/data/media/comics`).
- Set a consistent **folder format** (e.g. `$Series ($Year)`) and **file format**
  so issues land predictably for the reader.
- Downloads land in the client's completed dir; Mylar3's post-processor renames
  and moves them into the Comic Location. Make sure the download path is
  reachable by Mylar3 with the same mount layout as the client.

### 4. Handoff to the reader

Mylar3 only acquires/organizes; **Komga** or **Kavita** serve the library. Point
the reader's library at the same comics directory and trigger a scan (or rely on
its watcher) after Mylar3 post-processes new issues.

### 5. Mylar3 vs Kapowarr

Both can manage the same library — do not have both post-process the *same*
titles simultaneously. Mylar3 has stronger Prowlarr/usenet integration for
ongoing western comics; Kapowarr is ComicVine-driven and convenient for
back-catalogue volume grabs. Pick one owner per series.

## Port conflict note

If another service on the host already listens on `8090`, mylar3 will fail to
bind. Map it to a free host port instead — e.g. `-p 8091:8090/tcp` (or the
equivalent `ports:` entry in `compose.yml`).
