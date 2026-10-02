fn main() -> anyhow::Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments == ["--version"] {
        println!("gorak-lsp-rs {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if !arguments.is_empty() && arguments != ["--stdio"] {
        anyhow::bail!("Usage: gorak-lsp-rs [--stdio | --version]");
    }
    gorak_lsp_rs::server::run_stdio()
}
