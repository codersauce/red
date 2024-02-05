use std::{
    fs::{File, OpenOptions},
    io::Write,
    sync::Mutex,
};

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
