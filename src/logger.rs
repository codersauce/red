#![allow(unused)]
use std::{
    fmt,
    fs::{File, OpenOptions},
    io::Write,
    sync::Mutex,
    time::SystemTime,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogLevel::Debug => write!(f, "DEBUG"),
            LogLevel::Info => write!(f, "INFO"),
            LogLevel::Warn => write!(f, "WARN"),
            LogLevel::Error => write!(f, "ERROR"),
        }
    }
}

impl LogLevel {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "DEBUG" => Some(LogLevel::Debug),
            "INFO" => Some(LogLevel::Info),
            "WARN" => Some(LogLevel::Warn),
            "ERROR" => Some(LogLevel::Error),
            _ => None,
        }
    }
}

pub struct Logger {
    file: Mutex<File>,
    min_level: LogLevel,
}

impl Logger {
    pub fn new(file: &str) -> Self {
        let file = OpenOptions::new()
            .create(true)
            .append(true) // implies .write(true)
            .open(file)
            .expect("log file opens fine");

        Logger {
            file: Mutex::new(file),
            min_level: LogLevel::Debug, // Default to showing all logs
        }
    }

    pub fn set_level(&mut self, level: LogLevel) {
        self.min_level = level;
    }

    pub fn log(&self, message: &str) {
        self.log_with_level(LogLevel::Info, message);
    }

    pub fn log_with_level(&self, level: LogLevel, message: &str) {
        if level < self.min_level {
            return;
        }

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let formatted = format!("[{}] [{}] {}", timestamp, level, message);

        let mut file = self.file.lock().unwrap();
        writeln!(file, "{}", formatted).expect("write to file works");
    }

    pub fn debug(&self, message: &str) {
        self.log_with_level(LogLevel::Debug, message);
    }

    pub fn info(&self, message: &str) {
        self.log_with_level(LogLevel::Info, message);
    }

    pub fn warn(&self, message: &str) {
        self.log_with_level(LogLevel::Warn, message);
    }

    pub fn error(&self, message: &str) {
        self.log_with_level(LogLevel::Error, message);
    }
}
