use clap::{App, Arg};
use chrono::{Local, Timelike};
use std::fs::{File, OpenOptions, remove_file, symlink_metadata, read_link};
use std::os::unix::fs;
use std::io::{self, Write, Result, Read, BufRead};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

struct Config {
    folder: String,
    base_filename: String,
    gzip_on_rotate: bool,
}

fn main() -> std::io::Result<()> {
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
                .takes_value(true).required(true),
        )
        .arg(
            Arg::with_name("filename")
                .short('f')
                .long("filename")
                .value_name("FILENAME")
                .help("Specifies the base filename for log files")
                .takes_value(true).required(true),
        )
        .arg(
            Arg::with_name("compress")
                .short('c')
                .long("compress")
                .value_name("COMPRESS")
                .help("If supplied, indicates old log files should be compressed")
                .takes_value(false),
        ).get_matches();

    let config = Config {
        folder: matches.value_of("directory").unwrap().to_string(),
        base_filename: matches.value_of("filename").unwrap().to_string(),
        gzip_on_rotate: matches.is_present("compress"),
    };

    // Used to signal when the day changes; if true then the writer should rotate
    let date_changed = Arc::new(AtomicBool::new(true));
    let near_end_of_day = Arc::new(AtomicBool::new(true));

    // Start a thread to monitor when the local date changes.
    // When date does change, it sets date_changed to true.
    {
        let date_changed_clone = date_changed.clone();
        let near_end_of_day_clone = near_end_of_day.clone();

        thread::spawn(move || {
            let mut old_date = Local::now().date_naive();

            near_end_of_day_clone.store(true, Ordering::SeqCst);

            loop {
                // Once we near the end of the hour, poll more frequently
                if Local::now().minute() >= 59 {
                    thread::sleep(Duration::from_secs(1));
                } else {
                    near_end_of_day_clone.store(false, Ordering::SeqCst);
                    thread::sleep(Duration::from_secs(59));
                }

                let new_date = Local::now().date_naive();
                if old_date != new_date {
                    date_changed_clone.store(true, Ordering::SeqCst);
                    old_date = new_date;
                }
            }
        });
    }

    if cfg!(debug_assertions) {
        let date_changed_clone = date_changed.clone();

        thread::spawn(move || {
            use signal_hook::{iterator::Signals, consts::signal::SIGUSR1};

            let mut signals = Signals::new(&[SIGUSR1]).expect("Error setting up SIGUSR1 handler");

            for sig in signals.forever() {
                match sig {
                    SIGUSR1 => {
                        eprintln!("SIGUSR1 received; rotating log file");
                        date_changed_clone.store(true, Ordering::SeqCst);
                    }
                    _ => unreachable!(),
                }
            }
        });
    }

    let stdin = io::stdin();
    let mut buffer = Vec::with_capacity(8192);
    buffer.resize(8192, 0);

    let mut file = reopen_log_file(&config)?;

    loop {
        if near_end_of_day.load(Ordering::Relaxed) {
            let bytes_read = stdin.lock().read_until(b'\n', &mut buffer).expect("Error reading stdin");
            buffer.truncate(bytes_read);
        } else {
            let bytes_read = stdin.lock().read(&mut buffer).expect("Error reading stdin");
            buffer.truncate(bytes_read);
        }

        if buffer.len() == 0 {
            // Terminate once we reach EOF
            break;
        }

        // If we've been notified that the date has changed, rotate log files
        let has_date_changed = date_changed.load(Ordering::SeqCst);

        if has_date_changed {
            date_changed.store(false, Ordering::SeqCst);

            drop(file);

            file = reopen_log_file(&config)?;
        }

        file.write_all(&buffer)?;

        // If we only read 1 byte, sleep to let stdin fill up
        if buffer.len() == 1 {
            thread::sleep(Duration::from_millis(250));
        }
    }

    Ok(())
}

/// Opens the log file for the current date.
/// N.B. limitation is this always creates a date-stamped file, whereas really what we want to do is only do that on rotate...
fn reopen_log_file(config: &Config) -> Result<File> {
    let date_format;
    if cfg!(debug_assertions) {
        date_format = "%Y-%m-%d.%H%M%S";
    } else {
        date_format = "%Y-%m-%d";
    }

    let folder = Path::new(&config.folder);

    let link = folder.join(&config.base_filename);

    let formatted_date = Local::now().format(date_format).to_string();
    let filename = format!("{}-{}", config.base_filename, formatted_date);
    let filepath = folder.join(&filename);

    // Remove existing link
    let should_relink;
    if link.exists() {
        // Retrieve the metadata for the symlink
        let link_metadata = symlink_metadata(&link)?;

        // Check if the symlink points at
        if link_metadata.file_type().is_symlink() {
            let target_path = read_link(&link)?.canonicalize()?;
            let old_log_filepath = target_path.as_os_str().to_str().unwrap();

            should_relink = old_log_filepath != filepath.to_str().unwrap();

            if should_relink && config.gzip_on_rotate && !old_log_filepath.ends_with(".gz") {
                let old_log_file = old_log_filepath.to_owned();

                // Compress the old log file in the background
                thread::spawn(move || {
                    gzip_file_and_delete_original(&old_log_file);
                });
            }
        } else {
            should_relink = true;
        }

        if should_relink {
            if let Err(e) = remove_file(&link) {
                // If the link doesn't exist, don't consider it an error
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(e);
                }
            }
        }
    } else {
        should_relink = true;
    }

    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&filepath)?;

    if should_relink {
        if let Err(e) = fs::symlink(&filepath, &link) {
            return Err(e);
        }
    }

    Ok(file)
}


fn gzip_file_and_delete_original(file_path: &str) {
    if is_file_empty(file_path) {
        // Empty file, just delete; do not display errors if we cannot delete
        let _ = remove_file(file_path);
    } else {
        let gz_file_path = format!("{}.gz", file_path);

        let result = try_gzip_file(file_path, &gz_file_path);

        match result {
            Ok(_) => {
                if let Err(e) = remove_file(file_path) {
                    if Path::new(&file_path).exists() {
                        eprintln!("GZipped old log file after rotate, could not delete original: {}", e);
                    }
                }
            }
            Err(e) => {
                // Remove the .gz file if there was an error.
                if let Ok(_) = remove_file(&gz_file_path) {
                    eprintln!("Error gzipping old log file after rotate: {}", e);
                } else if Path::new(&gz_file_path).exists() {
                    eprintln!("Error gzipping old log file after rotate and unable to delete partial .gz file: {}", e);
                }
            }
        }
    }
}

fn is_file_empty(file_path: &str) -> bool {
    use std::fs;

    if let Ok(metadata) = fs::metadata(file_path) {
        return metadata.len() == 0;
    }
    false
}

fn try_gzip_file(src_file_path: &str, gz_file_path: &String) -> io::Result<()> {
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let mut input_file = File::open(src_file_path)?;
    let mut output_file = File::create(&gz_file_path)?;

    let mut encoder = GzEncoder::new(&mut output_file, Compression::default());
    let mut buffer = Vec::new();
    input_file.read_to_end(&mut buffer)?;

    // Write the buffer to the encoder and finish the encoding process.
    // If there's an error during the gzip process, it will be propagated.
    match encoder.write_all(&buffer).and(encoder.finish()) {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}
