use plugin_toolkit::prelude::*;

use crate::acquisition::{ClientProvider, ResolvedDownloadClient};
use crate::api::Mylar;
use crate::execute;
use crate::issues::{self, FailReport, QueueReport, WantedReport};
use crate::logs::{self, LogsReport};
use crate::reconcile::{self, ReconcileReport, ResolveReport};
use crate::remediate::{self, BacklogProcess, ConfigureReport};
use crate::status::{self, StatusReport};

#[endpoint_resource(plugin = "mylar3")]
pub struct Mylar3Endpoint {
    pub name: String,
    #[secret]
    pub api_key: String,
    /// Web UI login, needed to read settings when Mylar's web UI uses basic auth.
    pub web_username: Option<String>,
    #[secret]
    pub web_password: Option<String>,
    pub enabled: bool,
}

pub async fn connect(name: &str) -> Result<Mylar> {
    let row = endpoint_db::require(name)?;
    let api_key = plugin_toolkit::secrets::resolve_scoped("mylar3", name, "api_key")?;
    let web_auth = match row.web_username.filter(|u| !u.is_empty()) {
        Some(user) => {
            let pass = plugin_toolkit::secrets::resolve_scoped("mylar3", name, "web_password")?;
            Some((user, pass.expose_secret().clone()))
        }
        None => None,
    };
    let base_url = route::resolve_reachable(name, &row.routes, false).await?;
    Ok(Mylar::new(&base_url, api_key.expose_secret(), web_auth))
}

pub const DEFAULT_STUCK_HOURS: u32 = 24;

#[orca_struct(args)]
pub struct StatusArgs {
    /// Registered mylar3 endpoint name.
    #[arg(long)]
    pub name: String,
    /// A grab still at Snatched after this many hours counts as stuck.
    #[arg(long, default_value_t = DEFAULT_STUCK_HOURS)]
    #[serde(default = "default_stuck_hours")]
    pub stuck_hours: u32,
}

fn default_stuck_hours() -> u32 {
    DEFAULT_STUCK_HOURS
}

/// Detect settings drift and stuck grabs that silently break comic downloads.
#[orca_tool(domain = "mylar3", verb = "status", role = "read")]
async fn mylar3_status(args: StatusArgs, _ctx: &ToolCtx) -> Result<StatusReport> {
    let m = connect(&args.name).await?;
    status::status(&args.name, &m, args.stuck_hours).await
}

#[orca_struct(args)]
pub struct ReconcileArgs {
    /// Registered mylar3 endpoint name.
    #[arg(long)]
    pub name: String,
    /// Only grabs still at Snatched after this many hours are reconciled.
    #[arg(long, default_value_t = DEFAULT_STUCK_HOURS)]
    #[serde(default = "default_stuck_hours")]
    pub stuck_hours: u32,
    /// Stuck grabs to skip, oldest first.
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub offset: usize,
    /// Stuck grabs to return, at most 1000.
    #[arg(long, default_value_t = reconcile::DEFAULT_LIMIT)]
    #[serde(default = "default_reconcile_limit")]
    pub limit: usize,
}

fn default_reconcile_limit() -> usize {
    reconcile::DEFAULT_LIMIT
}

/// Match each stuck Snatched grab to SABnzbd's history (via the SAB host and key Mylar holds) and classify it.
#[orca_tool(domain = "mylar3", verb = "snatched.reconcile", role = "read")]
async fn mylar3_snatched_reconcile(args: ReconcileArgs, _ctx: &ToolCtx) -> Result<ReconcileReport> {
    let m = connect(&args.name).await?;
    reconcile::reconcile(&args.name, &m, args.stuck_hours, args.offset, args.limit).await
}

#[orca_struct(args)]
pub struct ResolveArgs {
    /// Registered mylar3 endpoint name.
    #[arg(long)]
    pub name: String,
    /// Only grabs still at Snatched after this many hours are resolved.
    #[arg(long, default_value_t = DEFAULT_STUCK_HOURS)]
    #[serde(default = "default_stuck_hours")]
    pub stuck_hours: u32,
    /// Also fail grabs SABnzbd has no history for.
    #[arg(long)]
    #[serde(default)]
    pub include_missing: bool,
}

/// Fail each stuck Snatched grab SABnzbd reports failed, so Mylar skips that release; completed ones are listed for `mylar3.backlog.process`.
#[orca_tool(
    domain = "mylar3",
    verb = "snatched.resolve",
    role = "admin",
    execute_gated = false,
    dry_run_aware = true
)]
async fn mylar3_snatched_resolve(args: ResolveArgs, ctx: &ToolCtx) -> Result<ResolveReport> {
    execute::require_admin("mylar3.snatched.resolve", ctx)?;
    let m = connect(&args.name).await?;
    reconcile::resolve(
        &args.name,
        &m,
        args.stuck_hours,
        args.include_missing,
        !ctx.dry_run(),
    )
    .await
}

#[orca_struct(args)]
pub struct LogsArgs {
    /// Registered mylar3 endpoint name.
    #[arg(long)]
    pub name: String,
    /// Case-insensitive substring a line must contain.
    #[arg(long)]
    #[serde(default)]
    pub grep: Option<String>,
    /// Newest lines to return, at most 2500.
    #[arg(long, default_value_t = logs::DEFAULT_LINES)]
    #[serde(default = "default_log_lines")]
    pub lines: usize,
    /// Newest matching lines to skip, for paging back through the log.
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub offset: usize,
}

fn default_log_lines() -> usize {
    logs::DEFAULT_LINES
}

/// Tail Mylar's log. Reads the web log table, since the API's getLogs is always empty on v0.11.
#[orca_tool(domain = "mylar3", verb = "logs", role = "read")]
async fn mylar3_logs(args: LogsArgs, _ctx: &ToolCtx) -> Result<LogsReport> {
    let m = connect(&args.name).await?;
    logs::logs(
        &args.name,
        &m,
        args.grep.as_deref(),
        args.offset,
        args.lines,
    )
    .await
}

#[orca_struct(args)]
pub struct ConfigureArgs {
    /// Registered mylar3 endpoint name.
    #[arg(long)]
    pub name: String,
    /// Days of usenet retention advertised to indexers (newznab `maxage`); omitted, raises it to reach the oldest Wanted issue, at least 6000.
    #[arg(long)]
    #[serde(default)]
    pub usenet_retention: Option<u32>,
    /// Torrent downloader to configure: qbittorrent, deluge (needs a username), transmission, rtorrent, utorrent or watchdir (no credentials).
    #[arg(long)]
    #[serde(default)]
    pub torrent_client: Option<String>,
    /// Torrent client base URL as Mylar reaches it, e.g. `http://10.0.0.16:8080`.
    #[arg(long)]
    #[serde(default)]
    pub torrent_url: Option<String>,
    /// Torrent client username.
    #[arg(long)]
    #[serde(default)]
    pub torrent_username: Option<String>,
    /// orca secret holding the torrent client password, not read on a dry run; defaults to `mylar3.torrent_password`.
    #[arg(long)]
    #[serde(default)]
    pub torrent_password_secret: Option<String>,
    /// Category or label Mylar files torrents under; defaults to `comics`.
    #[arg(long)]
    #[serde(default)]
    pub torrent_category: Option<String>,
    /// Folder the torrent client saves into, as the client sees it.
    #[arg(long)]
    #[serde(default)]
    pub torrent_directory: Option<String>,
    /// Turn Mylar's failed download handling on or off; on, its search skips releases marked failed.
    #[arg(long)]
    #[serde(default)]
    pub failed_download_handling: Option<bool>,
    /// Turn automatic re-search after a failed download on or off.
    #[arg(long)]
    #[serde(default)]
    pub failed_auto: Option<bool>,
}

/// Fix settings drift through Mylar's web settings form.
#[orca_tool(
    domain = "mylar3",
    verb = "configure",
    role = "admin",
    execute_gated = false,
    dry_run_aware = true
)]
async fn mylar3_configure(args: ConfigureArgs, ctx: &ToolCtx) -> Result<ConfigureReport> {
    const TOOL: &str = "mylar3.configure";
    execute::require_admin(TOOL, ctx)?;
    let dry_run = ctx.dry_run();
    let torrent = torrent_client(&args, dry_run, |name| {
        plugin_toolkit::secrets::get_required(name).map(|s| s.expose_secret().clone())
    })?;
    let toggles: Vec<(&str, bool)> = [
        ("failed_download_handling", args.failed_download_handling),
        ("failed_auto", args.failed_auto),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k, v)))
    .collect();
    let m = connect(&args.name).await?;
    let mut report = remediate::configure(
        &args.name,
        &m,
        args.usenet_retention,
        torrent.as_ref().map(|(c, _)| c),
        &toggles,
        !dry_run,
    )
    .await?;
    if let Some((_, Some(secret))) = torrent.filter(|_| dry_run) {
        for c in report
            .changes
            .iter_mut()
            .filter(|c| c.key.ends_with("_password"))
        {
            c.target = format!("will be set from secret {secret}");
        }
    }
    Ok(report)
}

const DRY_RUN_PASSWORD: &str = "dry-run-placeholder";

fn torrent_provider(name: &str) -> Result<ClientProvider> {
    Ok(match name {
        "qbittorrent" => ClientProvider::Qbittorrent,
        "deluge" => ClientProvider::Deluge,
        "transmission" => ClientProvider::Transmission,
        "rtorrent" => ClientProvider::Rtorrent,
        "utorrent" => ClientProvider::Utorrent,
        "watchdir" => ClientProvider::Watchdir,
        other => bail!("unknown torrent_client '{other}'"),
    })
}

fn torrent_client(
    args: &ConfigureArgs,
    dry_run: bool,
    get_secret: impl Fn(&str) -> Result<String>,
) -> Result<Option<(ResolvedDownloadClient, Option<String>)>> {
    let Some(client) = args.torrent_client.as_deref() else {
        return Ok(None);
    };
    let provider = torrent_provider(client)?;
    let username = args.torrent_username.as_deref().filter(|u| !u.is_empty());
    match (provider, username) {
        (ClientProvider::Watchdir, Some(_)) => {
            bail!("torrent_client 'watchdir' takes no torrent_username")
        }
        (ClientProvider::Deluge, None) => {
            bail!("torrent_client 'deluge' needs torrent_username and its password secret")
        }
        _ => {}
    }
    let secret = username.map(|_| {
        args.torrent_password_secret
            .clone()
            .unwrap_or_else(|| "mylar3.torrent_password".to_string())
    });
    let password = match (&secret, dry_run) {
        (Some(secret), false) => Some(get_secret(secret)?),
        (Some(_), true) => Some(DRY_RUN_PASSWORD.to_string()),
        (None, _) => None,
    };
    let (directory, client_directory) = match provider {
        ClientProvider::Watchdir => (args.torrent_directory.clone(), None),
        _ => (None, args.torrent_directory.clone()),
    };
    Ok(Some((
        ResolvedDownloadClient {
            provider: Some(provider),
            name: client.to_string(),
            url: args.torrent_url.clone(),
            username: args.torrent_username.clone(),
            password,
            category: args.torrent_category.clone(),
            directory,
            client_directory,
            ..Default::default()
        },
        secret,
    )))
}

#[orca_struct(args)]
pub struct BacklogArgs {
    /// Registered mylar3 endpoint name.
    #[arg(long)]
    pub name: String,
    /// Completed-downloads folder as Mylar's container sees it, e.g. `/downloads/completed/comics`.
    #[arg(long)]
    pub folder: String,
    /// Threshold for the stuck-at-Snatched count reported alongside.
    #[arg(long, default_value_t = DEFAULT_STUCK_HOURS)]
    #[serde(default = "default_stuck_hours")]
    pub stuck_hours: u32,
}

/// Queue Mylar's post-processor over a completed-downloads folder.
#[orca_tool(
    domain = "mylar3",
    verb = "backlog.process",
    role = "admin",
    execute_gated = false,
    dry_run_aware = true
)]
async fn mylar3_backlog_process(args: BacklogArgs, ctx: &ToolCtx) -> Result<BacklogProcess> {
    const TOOL: &str = "mylar3.backlog.process";
    execute::require_admin(TOOL, ctx)?;
    let m = connect(&args.name).await?;
    remediate::process_backlog(
        &args.name,
        &m,
        &args.folder,
        args.stuck_hours,
        !ctx.dry_run(),
    )
    .await
}

#[orca_struct(args)]
pub struct IssueArgs {
    /// Registered mylar3 endpoint name.
    #[arg(long)]
    pub name: String,
    /// Mylar IssueID; or give `series` and `issue_number`.
    #[arg(long)]
    #[serde(default)]
    pub issue_id: Option<String>,
    /// Watchlist series name (exact, case-insensitive) or ComicID.
    #[arg(long)]
    #[serde(default)]
    pub series: Option<String>,
    /// Issue number within `series`, e.g. `12`; for an annual, its number among the annuals.
    #[arg(long)]
    #[serde(default)]
    pub issue_number: Option<String>,
    /// Address the series' annual numbered `issue_number` instead of the regular issue.
    #[arg(long)]
    #[serde(default)]
    pub annual: bool,
}

/// Mark an issue Wanted and request a search for it; a story-arc issue id (`StoryArcID_IssueArcID`) also needs `series` and `issue_number` as listed in the arc.
#[orca_tool(
    domain = "mylar3",
    verb = "issue.queue",
    role = "admin",
    execute_gated = true
)]
async fn mylar3_issue_queue(args: IssueArgs, ctx: &ToolCtx) -> Result<QueueReport> {
    execute::require_admin("mylar3.issue.queue", ctx)?;
    let m = connect(&args.name).await?;
    let target = issues::resolve(
        &m,
        args.issue_id.as_deref(),
        args.series.as_deref(),
        args.issue_number.as_deref(),
        args.annual,
    )
    .await?;
    issues::queue(&args.name, &m, target).await
}

/// Fail an issue's Snatched release so Mylar's search skips it; the issue stays Failed, and with `failed_auto` Mylar requests a search skipping that release.
#[orca_tool(
    domain = "mylar3",
    verb = "issue.fail",
    role = "admin",
    execute_gated = true
)]
async fn mylar3_issue_fail(args: IssueArgs, ctx: &ToolCtx) -> Result<FailReport> {
    execute::require_admin("mylar3.issue.fail", ctx)?;
    let m = connect(&args.name).await?;
    let target = issues::resolve(
        &m,
        args.issue_id.as_deref(),
        args.series.as_deref(),
        args.issue_number.as_deref(),
        args.annual,
    )
    .await?;
    issues::fail(&args.name, &m, target).await
}

#[orca_struct(args)]
pub struct WantedArgs {
    /// Registered mylar3 endpoint name.
    #[arg(long)]
    pub name: String,
    /// Wanted issues to skip.
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub offset: usize,
    /// Wanted issues to return, at most 1000.
    #[arg(long, default_value_t = issues::DEFAULT_LIMIT)]
    #[serde(default = "default_wanted_limit")]
    pub limit: usize,
}

fn default_wanted_limit() -> usize {
    issues::DEFAULT_LIMIT
}

/// List the issues and annuals Mylar has marked Wanted.
#[orca_tool(domain = "mylar3", verb = "wanted.list", role = "read")]
async fn mylar3_wanted_list(args: WantedArgs, _ctx: &ToolCtx) -> Result<WantedReport> {
    let m = connect(&args.name).await?;
    issues::wanted(&args.name, &m, args.offset, args.limit).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_toolkit::contract::config::{Config, Model};
    use plugin_toolkit::contract::CallerIdentity;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn ctx(role: Option<&str>) -> ToolCtx {
        let cfg = Arc::new(Config {
            anthropic_api_key: None,
            lmstudio_url: String::new(),
            ollama_url: String::new(),
            default_model: Model::LMStudio {
                id: String::new(),
                url: String::new(),
            },
            app_dir: PathBuf::from("/nonexistent"),
            memory_root: PathBuf::from("/nonexistent"),
            db_path: PathBuf::from("/nonexistent/orca.db"),
            ports: Default::default(),
        });
        let ctx = ToolCtx::new(cfg);
        match role {
            Some(role) => ctx.with_auth(CallerIdentity {
                user_id: "u".into(),
                username: "op".into(),
                role: role.into(),
                can_mutate: true,
            }),
            None => ctx,
        }
    }

    fn dry_ctx(role: Option<&str>, dry_run: bool) -> ToolCtx {
        let mut ctx = ctx(role);
        ctx.set_dry_run(dry_run);
        ctx
    }

    #[test]
    fn writes_act_by_default_and_honour_dry_run() {
        let aware = plugin_toolkit::dispatch::dry_run_aware_names();
        for n in [
            "mylar3.snatched.resolve",
            "mylar3.configure",
            "mylar3.backlog.process",
        ] {
            assert!(aware.contains(&n), "{n}");
        }
    }

    #[tokio::test]
    async fn configure_requires_admin_with_and_without_dry_run() {
        for (role, dry_run) in [
            (Some("user"), false),
            (Some("user"), true),
            (None, false),
            (None, true),
        ] {
            let args = ConfigureArgs {
                name: "m".into(),
                usenet_retention: None,
                torrent_client: None,
                torrent_url: None,
                torrent_username: None,
                torrent_password_secret: None,
                torrent_category: None,
                torrent_directory: None,
                failed_download_handling: None,
                failed_auto: None,
            };
            let err = mylar3_configure(args, &dry_ctx(role, dry_run))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.starts_with("mylar3.configure"), "{err}");
        }
    }

    #[test]
    fn torrent_provider_names() {
        assert_eq!(
            torrent_provider("qbittorrent").unwrap(),
            ClientProvider::Qbittorrent
        );
        assert_eq!(
            torrent_provider("watchdir").unwrap(),
            ClientProvider::Watchdir
        );
        assert!(torrent_provider("sabnzbd").is_err());
    }

    fn torrent_args(username: Option<&str>, secret: Option<&str>) -> ConfigureArgs {
        ConfigureArgs {
            name: "m".into(),
            usenet_retention: None,
            torrent_client: Some("qbittorrent".into()),
            torrent_url: Some("http://10.0.0.16:8080".into()),
            torrent_username: username.map(str::to_string),
            torrent_password_secret: secret.map(str::to_string),
            torrent_category: None,
            torrent_directory: None,
            failed_download_handling: None,
            failed_auto: None,
        }
    }

    fn vault(name: &str) -> Result<String> {
        match name {
            "mylar3.torrent_password" => Ok("DEFAULT".into()),
            "mylar3.m.torrent_password" => Ok("SCOPED".into()),
            other => bail!("no secret named '{other}'"),
        }
    }

    #[test]
    fn torrent_client_reads_the_plugin_scoped_secret_by_default() {
        let (c, secret) = torrent_client(&torrent_args(Some("admin"), None), false, vault)
            .unwrap()
            .unwrap();
        assert_eq!(secret.as_deref(), Some("mylar3.torrent_password"));
        assert_eq!(c.password.as_deref(), Some("DEFAULT"));
    }

    #[test]
    fn torrent_client_reads_an_explicit_secret() {
        let args = torrent_args(Some("admin"), Some("mylar3.m.torrent_password"));
        let (c, _) = torrent_client(&args, false, vault).unwrap().unwrap();
        assert_eq!(c.password.as_deref(), Some("SCOPED"));
    }

    #[test]
    fn torrent_client_fails_on_a_missing_secret() {
        let args = torrent_args(Some("admin"), Some("absent"));
        let err = torrent_client(&args, false, vault).unwrap_err().to_string();
        assert!(err.contains("absent"), "{err}");
    }

    #[test]
    fn torrent_client_dry_run_does_not_read_the_secret() {
        let args = torrent_args(Some("admin"), Some("absent"));
        let (c, secret) = torrent_client(&args, true, |_| bail!("read on dry run"))
            .unwrap()
            .unwrap();
        assert_eq!(secret.as_deref(), Some("absent"));
        assert_eq!(c.password.as_deref(), Some(DRY_RUN_PASSWORD));
    }

    #[test]
    fn torrent_client_without_username_sets_no_credentials() {
        let (c, secret) = torrent_client(&torrent_args(None, None), false, |_| {
            bail!("read without a username")
        })
        .unwrap()
        .unwrap();
        assert!(secret.is_none() && c.username.is_none() && c.password.is_none());
    }

    #[test]
    fn torrent_client_rejects_credentials_it_cannot_use_before_the_secret() {
        let no_read = |_: &str| -> Result<String> { bail!("secret read") };
        let mut args = torrent_args(Some("admin"), None);
        args.torrent_client = Some("watchdir".into());
        let err = torrent_client(&args, false, no_read)
            .unwrap_err()
            .to_string();
        assert!(err.contains("takes no torrent_username"), "{err}");
        let mut args = torrent_args(None, None);
        args.torrent_client = Some("deluge".into());
        let err = torrent_client(&args, false, no_read)
            .unwrap_err()
            .to_string();
        assert!(err.contains("needs torrent_username"), "{err}");
    }

    #[test]
    fn torrent_client_watchdir_uses_the_directory() {
        let mut args = torrent_args(None, None);
        args.torrent_client = Some("watchdir".into());
        args.torrent_directory = Some("/watch".into());
        let (c, _) = torrent_client(&args, false, vault).unwrap().unwrap();
        assert_eq!(c.provider, Some(ClientProvider::Watchdir));
        assert_eq!(c.directory.as_deref(), Some("/watch"));
        assert!(c.client_directory.is_none());
    }

    #[tokio::test]
    async fn backlog_requires_admin_with_and_without_dry_run() {
        for (role, dry_run) in [
            (Some("user"), false),
            (Some("user"), true),
            (None, false),
            (None, true),
        ] {
            let args = BacklogArgs {
                name: "m".into(),
                folder: "/downloads/complete".into(),
                stuck_hours: DEFAULT_STUCK_HOURS,
            };
            let err = mylar3_backlog_process(args, &dry_ctx(role, dry_run))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.starts_with("mylar3.backlog.process"), "{err}");
        }
    }

    #[tokio::test]
    async fn resolve_requires_admin_with_and_without_dry_run() {
        for (role, dry_run) in [
            (Some("user"), false),
            (Some("user"), true),
            (None, false),
            (None, true),
        ] {
            let args = ResolveArgs {
                name: "m".into(),
                stuck_hours: DEFAULT_STUCK_HOURS,
                include_missing: false,
            };
            let err = mylar3_snatched_resolve(args, &dry_ctx(role, dry_run))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.starts_with("mylar3.snatched.resolve"), "{err}");
        }
    }

    #[tokio::test]
    async fn issue_writes_require_admin() {
        for role in [Some("user"), None] {
            let args = || IssueArgs {
                name: "m".into(),
                issue_id: Some("1".into()),
                series: None,
                issue_number: None,
                annual: false,
            };
            let err = mylar3_issue_queue(args(), &ctx(role))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.starts_with("mylar3.issue.queue"), "{err}");
            let err = mylar3_issue_fail(args(), &ctx(role))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.starts_with("mylar3.issue.fail"), "{err}");
        }
    }
}
