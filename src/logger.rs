#![allow(unused)]
use std::{
    fs::{File, OpenOptions},
    io::Write,
    sync::Mutex,
};

use once_cell::sync::OnceCell;

#[allow(unused)]
pub static LOGGER: OnceCell<Logger> = OnceCell::new();

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        {
            let log_message = format!($($arg)*);
            $crate::logger::LOGGER.get_or_init(|| $crate::Logger::new("red.log")).log(&log_message);
        }
    };
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
