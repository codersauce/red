pub fn init() -> anyhow::Result<()> {
    let (log_file, _) = tempfile::Builder::new().prefix("red-")
        .rand_bytes(8).suffix(".log").append(true).tempfile()?.keep()?;


    let env = env_logger::Env::new().filter("RED_LOG").write_style("RED_STYLE");

    Ok(env_logger::Builder::from_env(env).target(env_logger::Target::Pipe(Box::new(log_file))).init())
}
