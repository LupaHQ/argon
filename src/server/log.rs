// argon/src/server/log.rs
use actix_msgpack::MsgPack;
use actix_web::{
    post,
    web::{Data},
    HttpResponse, Responder,
};
use std::{
    fs::OpenOptions,
    io::{ErrorKind, Write},
    sync::Arc,
};

use crate::{core::Core}; // Removed lock import
use log::warn; // Use warn for logging errors here

const SESSION_START_MARKER: &str = "__ARGON_LOG_SESSION_START__";
const SESSION_START_HEADER: &str = "--- Argon Log Session Started: "; // Timestamp will be added
const LOG_FILE_PATH: &str = "lemonlogs.txt"; // Changed path

#[post("/log")]
async fn main(_core: Data<Arc<Core>>, body: MsgPack<String>) -> impl Responder {
    let log_message = body.0; // Access inner value using .0, not into_inner()

    // Use hardcoded path relative to workspace root
    let log_file_path = _core.project().workspace_dir.join(LOG_FILE_PATH);

     if log_message == SESSION_START_MARKER {
        // Clear the file and write header
        match OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_file_path)
        {
            Ok(mut file) => {
                let timestamp = chrono::Local::now().to_rfc3339();
                let header = format!("{}{}]\n", SESSION_START_HEADER, timestamp);
                if let Err(e) = writeln!(file, "{}", header.trim_end()) {
                    warn!( // Use warn! macro
                        "Error writing session start header to {}: {}",
                        log_file_path.display(),
                        e
                    );
                    return HttpResponse::InternalServerError().body("Failed to write log header");
                }
                HttpResponse::Ok().body("Log session started")
            }
            Err(e) => {
                warn!( // Use warn! macro
                    "Error opening/creating {} for truncation: {}",
                    log_file_path.display(),
                    e
                );
                HttpResponse::InternalServerError().body("Failed to open log file")
            }
        }
    } else {
        // Append the log message
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file_path)
        {
            Ok(mut file) => {
                if let Err(e) = writeln!(file, "{}", log_message) {
                    warn!("Error writing to {}: {}", log_file_path.display(), e); // Use warn! macro
                    return HttpResponse::InternalServerError().body("Failed to write log");
                }
                HttpResponse::Ok().body("Log received")
            }
            Err(e) => {
                // Specifically handle case where file doesn't exist yet after a clear potentially failed
                if e.kind() == ErrorKind::NotFound {
                     warn!( // Use warn! macro
                        "Log file {} not found for append, might need session start first.",
                        log_file_path.display()
                    );
                     // Optionally try to create it here? Or just let the next SESSION_START handle it.
                     // For now, just report error.
                     return HttpResponse::InternalServerError().body("Log file not found, session might not be started");

                } else {
                    warn!("Error opening {} for append: {}", log_file_path.display(), e); // Use warn! macro
                    return HttpResponse::InternalServerError().body("Failed to open log file");
                }
            }
        }
    }
} 