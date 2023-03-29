use clap::{App, Arg};
use chrono::{Local, NaiveDate};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

struct Config {
    folder: String,
    base_filename: String,
}

fn main() {
    let matches = App::new("rotatelog")
        .version("1.0")
        .author("Peter Wright")
        .about("Automatic date-based log rotation utility")
        .arg(
            Arg::with_name("directory")
                .short('d')
                .long("directory")
                .value_name("DIRECTORY")
                .help("Specifies the folder for log files")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("filename")
                .short('f')
                .long("filename")
                .value_name("FILENAME")
                .help("Specifies the base filename for log files")
                .takes_value(true),
        )
        .get_matches();

    if matches.is_present("help") || matches.is_present("version") || !matches.is_present("directory") || !matches.is_present("base_filename") {
        //println!("{}", matches.usage());
        println!("Usage...");
        return;
    }

    let config = Config {
        folder: matches.value_of("folder").unwrap().to_string(),
        base_filename: matches.value_of("filename").unwrap().to_string(),
    };

    let date_changed = Arc::new(AtomicBool::new(false));
    let date_changed_clone = date_changed.clone();

    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(60));

        let old_date = Local::today().naive_local();
        let new_date = Local::now().date().naive_local();
        if old_date != new_date {
            date_changed_clone.store(true, Ordering::SeqCst);
        }
    });

    let mut current_date = Local::today().naive_local();
    let stdin = io::stdin();
    let mut buffer = String::new();

    loop {
        buffer.clear();
        let bytes_read = stdin.lock().read_line(&mut buffer).expect("Error reading stdin");

        if bytes_read == 0 {
            break;
        }

        // If we've been notified that the date has changed, rotate log files
        // TODO should probably only do this rotation after writing a newline char?
        if date_changed.load(Ordering::SeqCst) {
            current_date = Local::today().naive_local().pred();
            date_changed.store(false, Ordering::SeqCst);
        }

        let filepath = format!("{}/{}-{}.log", config.folder, config.base_filename, current_date);
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&filepath)
            .expect("Error opening/creating log file");

        file.write_all(buffer.as_bytes()).expect("Error writing to log file");
    }
}
