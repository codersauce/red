#![allow(unused)]
use std::{
    fs::{File, OpenOptions},
    io::Write,
    sync::Mutex,
};

pub fn init() -> anyhow::Result<()> {
    let (log_file, _) = tempfile::Builder::new().prefix("red-")
        .rand_bytes(8).suffix(".log").append(true).tempfile()?.keep()?;


    let env = env_logger::Env::new().filter("RED_LOG").write_style("RED_STYLE");

    Ok(env_logger::Builder::from_env(env).target(env_logger::Target::Pipe(Box::new(log_file))).init())
}

pub struct Logger {
    file: Mutex<File>,
}

impl Logger {
    pub fn new(file: &str) -> Self {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .append(true)
            .open(file)
            .expect("log file opens fine");

        Logger {
            file: Mutex::new(file),
        }
    }

    pub fn log(&self, message: &str) {
        let mut file = self.file.lock().unwrap();
        writeln!(file, "{}", message).expect("write to file works");
    }
}
