# mylar3 — ServiceBackend contract

Pure-Rust plugin (**no bash/compose/provision scripts**) driven by the single
generic `service.*` surface. Runtimes: **docker,podman,lxc**.

## Per-plugin code (the only work this repo owns)
- [x] `provider` / `runtimes` / `default_port` / `capabilities` / `data_paths` — declarative descriptor
- [ ] `workload_spec(runtime)` — *what* to run; `deploy_target` renders it to a container / LXC / VM
- [ ] `configure` — apply mylar3 config via its upstream API
- [ ] `status` — health + rich diagnostics returned in the typed `ServiceStatus.info`

> Declarative descriptor is implemented and the plugin **registers + loads live**
> in orca today (`service.list` shows it). `workload_spec`/`configure`/`status`
> are being filled in per plugin.

## Tools (`mylar3.`)
- [x] endpoint registry `mylar3.{list,detail,create,update,delete}` — routes + api_key (secret) + web login (password secret)
- [x] `status` — usenet retention vs oldest Wanted issue, completed-download handling, issues stuck at Snatched, search delay, torrent search without a client
- [x] `configure` — converge `usenet_retention` (and `nzb_downloader` when SABnzbd is set but unused) through the web settings form, plus `failed_download_handling` / `failed_auto` on request; dry run by default
- [x] `backlog.process` — queue Mylar's post-processor over a folder inside `sab_directory`/`check_folder`, clear of the library; dry run by default
- [x] `issue.queue` — mark an issue Wanted, search requested (API `queueIssue`), by IssueID or series + issue number (`getIndex` + `getComic`); `--annual` queues the series' annual via web `/queueit?mode=want_ann`; a story-arc id (`StoryArcID_IssueArcID`) + series + issue number queues via web `/queueit?mode=readlist` after `getStoryArc` (refused when series + number match several arc rows or the row has no issue date), then reads the status back via `getWanted&story_arcs=true` (needs `upcoming_storyarcs`); a bare story-arc id is refused; executes by default, `dryRun` previews
- [x] `issue.fail` — fail an issue's Snatched release (web `/failed_handling`) so Mylar's search skips it; the issue stays Failed, and with `failed_auto` Mylar requests a search skipping that release; re-reads the issue and errors if unchanged; refuses annuals, story-arc ids, `failed_download_handling` off, or no Snatched grab
- [x] `wanted.list` — Wanted issues and annuals with ComicID/IssueID (API `getWanted`), paged

## Provided generically by orca (NO code here)
- `deploy` — `service.deploy` → `deploy_target.launch(WorkloadSpec)`
- `backup` / `restore` — pluggable `BackupMethod` (tar; **PBS** for Proxmox guests)
- single `service.*` tool surface, exposed over CLI / REST / MCP
