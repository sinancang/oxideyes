use chrono::Local;
use clap::Parser;
use crossbeam_channel::unbounded;
use env_logger::Builder;
use log::{LevelFilter, debug, error, info};
use rdev::{Event, EventType, listen};
use serde_json::{Value, json, to_string_pretty};
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Seek, Write};
use std::sync::{Arc, Mutex};
use std::thread;

mod processing;
use crate::processing::{merge_stats, process_event};

mod types;
use crate::types::{Cli, Config, Stats};

fn init_logger(log_level: &str) {
    let level = match log_level.to_lowercase().as_str() {
        "trace" => LevelFilter::Trace,
        "debug" => LevelFilter::Debug,
        "info" => LevelFilter::Info,
        "warn" => LevelFilter::Warn,
        "error" => LevelFilter::Error,
        _ => LevelFilter::Info, // default
    };

    Builder::new().filter(None, level).init();
}

fn main() {
    let args = Cli::parse();
    init_logger(&args.log_level);

    if !std::path::Path::new(&args.config).exists() {
        error!("Config file does not exist: {}", args.config);
        std::process::exit(1);
    }
    let config_contents = fs::read_to_string(args.config.clone())
        .unwrap_or_else(|_| panic!("Failed to read config file: {}", args.config));
    let config: Config =
        toml::from_str(&config_contents).expect("Failed to parse config from TOML");

    let stats_postfix: String = config.stats.postfix;
    let stats_dir: String = config.stats.dir;
    if !std::path::Path::new(&stats_dir).exists() {
        fs::create_dir_all(&stats_dir).expect("Failed to create stats directory");
    }
    if !std::path::Path::new(&stats_dir).is_dir() {
        error!("Stats directory is not a directory: {}", stats_dir);
        std::process::exit(1);
    }
    let log_period: u64 = config.stats.period_ms;
    info!("Log level: {}", args.log_level);
    info!(
        "Logging to directory '{}' every {} milliseconds",
        stats_dir, log_period
    );

    let state_for_event_listener = Arc::new(Mutex::new(Stats::default()));
    let state_for_logger = Arc::clone(&state_for_event_listener);

    thread::spawn(move || {
        logger_thread(stats_dir, stats_postfix, log_period, state_for_logger);
    });

    let (tx, rx) = unbounded::<EventType>();
    thread::spawn(move || {
        event_listener(state_for_event_listener, rx);
    });

    let callback = {
        move |ev: Event| {
            tx.send(ev.event_type).ok();
        }
    };
    match listen(callback) {
        Ok(_) => {}
        Err(error) => {
            error!("Error occurred while listening for events: {:?}", error);
        }
    }
}

fn event_listener(stats: Arc<Mutex<Stats>>, rx: crossbeam_channel::Receiver<EventType>) {
    let mut last_mouse_pos = (0.0, 0.0);
    for event in rx {
        let mut s = stats.lock().expect("Mutex poisoned while locking state");
        process_event(event, &mut s, &mut last_mouse_pos);
    }
}

fn logger_thread(
    stats_dir: String,
    stats_postfix: String,
    log_period: u64,
    state: Arc<Mutex<Stats>>,
) {
    loop {
        thread::sleep(std::time::Duration::from_millis(log_period));

        let mut s = state.lock().expect("Mutex poisoned while locking state");

        // Create updated log object
        let date = Local::now().format("%Y%m%d").to_string();
        let time = Local::now().timestamp();
        let json_update = json!({
            "timestamp": time,
            "mouse_distance": s.mouse_distance,
            "wheel_spins": s.wheel_distance,
            "button_presses": s.button_presses,
            "key_presses": s.key_presses
        });
        debug!("Updating JSON with: {}", json_update);

        let file_path = format!("{}/{}_{}.json", stats_dir, date, stats_postfix);
        debug!("Log file path: {}", file_path);

        // Open and read file
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&file_path)
            .expect("Failed to open log file");

        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .expect("Failed to read file");

        let existing: Value = if contents.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&contents)
                .expect("Failed to parse existing JSON")
        };

        let data = merge_stats(existing, &json_update);

        file.set_len(0).expect("Failed to truncate file");
        file.seek(std::io::SeekFrom::Start(0))
            .expect("Failed to seek start");

        let pretty = to_string_pretty(&data).expect("Failed to serialize JSON");
        write!(file, "{}", pretty).expect("Failed to write updated JSON");

        *s = Stats::default();
    }
}
