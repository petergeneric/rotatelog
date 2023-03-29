use clap::{App, Arg};
use chrono::{Local, NaiveDate, Timelike};
use std::fs::{File, OpenOptions, remove_file};
use std::os::unix::fs;
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

    if matches.is_present("help") || matches.is_present("version") || !matches.is_present("directory") || !matches.is_present("filename") {
        //println!("{}", matches.usage());
        println!("Usage...");
        return;
    }

    let config = Config {
        folder: matches.value_of("directory").unwrap().to_string(),
        base_filename: matches.value_of("filename").unwrap().to_string(),
    };

    // Used to signal when the day changes; if true then the writer should rotate
    let date_changed = Arc::new(AtomicBool::new(true));

    // Start a thread to monitor when the local date changes.
    {
        let date_changed_clone = date_changed.clone();
        thread::spawn(move || {
            let mut old_date = Local::now().date_naive();

            loop {
                // Once we near the end of the hour, poll more frequently
                if Local::now().minute() >= 59 {
                    thread::sleep(Duration::from_secs(1));
                } else {
                    thread::sleep(Duration::from_secs(60));
                }

                let new_date = Local::now().date_naive();
                if old_date != new_date {
                    date_changed_clone.store(true, Ordering::SeqCst);
                    old_date = new_date;
                }
            }
        });
    }

    let stdin = io::stdin();
    let mut buffer = String::new();

    let mut file = open_log_file(&config);


    loop {
        buffer.clear();

        // Read all available log data
        let bytes_read = stdin.lock().read_line(&mut buffer).expect("Error reading stdin");

        // Terminate once we reach EOF
        if bytes_read == 0 {
            break;
        }

        // If we've been notified that the date has changed, rotate log files
        if date_changed.load(Ordering::SeqCst) {
            date_changed.store(false, Ordering::SeqCst);

            file = open_log_file(&config);
            // TODO recreate link to latest log file (symlink likely best)

        }

        file.write_all(buffer.as_bytes()).expect("Error writing to log file");
    }
}

/// Opens the log file for the current date.
/// N.B. limitation is this always creates a date-stamped file, whereas really what we want to do is only do that on rotate...
fn open_log_file(config: &Config) -> File {
    let current_date = Local::now().date_naive();

    let filepath = format!("{}/{}-{}", config.folder, config.base_filename, current_date);
    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&filepath)
        .expect("Error opening/creating log file");

    let link = format!("{}/{}", config.folder, config.base_filename);

    // TODO handle errors
    remove_file(&link);
    fs::symlink(filepath, &link);

    file
}
