fn main() -> anyhow::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("Pass the application executable path"))?;
    let path = std::path::PathBuf::from(path).canonicalize()?;
    let updater = spotifast::updates::updater(&spotifast::settings::ProxyConfig::System)?;
    match updater.installation_at(&path) {
        Ok(installation) => println!("{}", serde_json::to_string(&installation)?),
        Err(error) => println!("Managed or unsupported: {error}"),
    }
    Ok(())
}
