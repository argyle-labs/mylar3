plugin_toolkit::instrument::bootstrap!();
use mylar3::Mylar3Backend;
use plugin_toolkit::plugin::Plugin;

#[allow(unused_imports)]
use mylar3::tools as _force_link_tool_registrations;

fn main() -> plugin_toolkit::anyhow::Result<()> {
    Plugin::named("mylar3")
        .version(env!("CARGO_PKG_VERSION"))
        .service(Mylar3Backend::new("mylar3"))
        .tools(["mylar3."])
        .serve()
}
