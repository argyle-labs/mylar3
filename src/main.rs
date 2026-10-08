//! Dynamic (subprocess) entrypoint for the mylar3 plugin.
//!
//! Builds the typed `Plugin` and serves it over the orca socket. The plugin is
//! a `[[bin]]`, owns no runtime, and reaches orca only through the socket.
plugin_toolkit::instrument::bootstrap!();
use mylar3::Mylar3Backend;
use plugin_toolkit::plugin::Plugin;

// The builder does not force-link the lib, and without this the linker drops
// every `#[orca_tool]` / `#[endpoint_resource]` registration.
#[allow(unused_imports)]
use mylar3::tools as _;

fn main() -> plugin_toolkit::anyhow::Result<()> {
    Plugin::named("mylar3")
        .version(env!("CARGO_PKG_VERSION"))
        .service(Mylar3Backend::new("mylar3"))
        .tools(["mylar3."])
        .serve()
}
