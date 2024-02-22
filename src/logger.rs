use std::path;

pub fn init() -> anyhow::Result<path::PathBuf> {
    let (log_file, path) = tempfile::Builder::new().prefix("red-")
        .rand_bytes(8).suffix(".log").append(true).tempfile()?.keep()?;

    let env = env_logger::Env::new().filter("RED_LOG").write_style("RED_STYLE");

    env_logger::Builder::from_env(env).target(env_logger::Target::Pipe(Box::new(log_file))).init();

    Ok(path)
}
