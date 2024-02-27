//! Tauri app for Zebra

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};

use tauri::Manager;

// Learn more about Tauri commands at https://tauri.app/v1/guides/features/command
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

fn main() {
    // Spawn initial zebrad process
    let mut zebrad = Command::new("zebrad")
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("zebrad should be installed as a bundled binary and should start successfully");

    // Spawn a task for reading output and sending it to a channel
    let (zebrad_log_sender, mut zebrad_log_receiver) = tokio::sync::mpsc::channel(100);
    let zebrad_stdout = zebrad.stdout.take().expect("should have anonymous pipe");
    let _log_emitter_handle = std::thread::spawn(move || {
        for line in BufReader::new(zebrad_stdout).lines() {
            // Ignore send errors for now
            let _ =
                zebrad_log_sender.blocking_send(line.expect("zebrad logs should be valid UTF-8"));
        }
    });

    tauri::Builder::default()
        .setup(|app| {
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    if let Some(output) = zebrad_log_receiver.recv().await {
                        app_handle.emit("log", output).unwrap();
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
