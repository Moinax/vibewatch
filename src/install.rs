use anyhow::Result;

pub struct Options {
    pub no_service: bool,
    pub no_hooks: bool,
    pub dry_run: bool,
    pub uninstall: bool,
}

pub fn run(opts: Options) -> Result<()> {
    if opts.uninstall {
        eprintln!("vibewatch install: uninstall not implemented yet");
    } else {
        eprintln!("vibewatch install: not implemented yet");
    }
    // Consume unused fields so clippy stays quiet.
    let _ = (opts.no_service, opts.no_hooks, opts.dry_run);
    Ok(())
}
