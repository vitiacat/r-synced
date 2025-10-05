mod utils;

use crate::utils::{format_bytes, parse_rsync_progress};
use anyhow::Context;
use eframe::egui;
use eframe::egui::{Checkbox, ProgressBar, Vec2};
use lazy_static::lazy_static;
use nix::sys::signal;
use nix::sys::signal::Signal;
use nix::unistd::Pid;
use regex::Regex;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::thread;

#[derive(Default)]
struct Progress {
    total_progress: f32,
    progress: f32,

    speed: String,
    time: String,
    bytes_sent: u64,
}

#[derive(Default)]
struct Finished {}

#[derive(Default)]
struct NextFile {
    line: String,
}

#[derive(Default)]
struct Error {
    line: String,
}

enum StateMessage {
    Progress(Progress),
    NextFile(NextFile),
    Finished(Finished),
    Error(Error),
}

#[derive(Default)]
struct AppState {
    src: String,
    dest: String,
    progress: Option<Receiver<StateMessage>>,
    logs: String,
    error_logs: String,
    current_progress: Progress,
    is_finished: bool,
    child: Option<Child>,

    archive: bool,
    recursive: bool,
    symlinks: bool,
    permissions: bool,
    time: bool,
    group: bool,
    compress: bool,
    excluded: Vec<String>,
}

fn create_rsync_command(state: &AppState) -> Command {
    let mut cmd = Command::new("rsync");

    cmd.arg("-i");
    cmd.arg("--progress");

    if state.archive {
        cmd.arg("-a");
    } else {
        if state.recursive {
            cmd.arg("-r");
        }
        if state.symlinks {
            cmd.arg("-l");
        }
        if state.permissions {
            cmd.arg("-p");
        }
        if state.time {
            cmd.arg("-t");
        }
        if state.group {
            cmd.arg("-g");
        }
    }

    if state.compress {
        cmd.arg("-z");
    }

    cmd.arg(&state.src);
    cmd.arg(&state.dest);

    cmd
}

fn create_rsync_dry_run_command(state: &AppState) -> Command {
    let mut cmd = Command::new("rsync");

    cmd.arg("-e")
        .arg("ssh -o PasswordAuthentication=no -o PreferredAuthentications=publickey");
    cmd.arg("-an");
    cmd.arg("--stats");

    cmd.arg(state.src.clone());
    cmd.arg(state.dest.clone());

    cmd
}

fn run_rsync(
    mut cmd: Command,
    files_count: u64,
    ctx: egui::Context,
) -> (Receiver<StateMessage>, Child) {
    let (tx, rx) = mpsc::channel::<StateMessage>();

    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("");
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let err_reader = BufReader::new(stderr);
    let mut buffer = Vec::new();

    let cloned_tx = tx.clone();

    thread::spawn(move || {
        for line in err_reader.lines() {
            if let Ok(line) = line {
                cloned_tx.send(StateMessage::Error(Error { line })).unwrap();
            }
        }
    });

    thread::spawn(move || {
        let mut count = 0;
        let mut data = (String::from("N/A"), String::from("N/A"), 0, 0);

        while let Ok(bytes_read) = reader.read_until(b'\r', &mut buffer) {
            if bytes_read == 0 {
                break;
            }

            if let Ok(line_str) = str::from_utf8(&buffer) {
                let trimmed_line = line_str.trim_end_matches(['\r', '\n']).trim();
                let lines = trimmed_line.lines();

                for line in lines {
                    let p = parse_rsync_progress(line);
                    if let Some(progress) = p {
                        data = (
                            progress.speed,
                            progress.estimated_time,
                            progress.bytes_transferred,
                            progress.percentage,
                        );

                        tx.send(StateMessage::Progress(Progress {
                            progress: data.3 as f32 / 100.0,
                            total_progress: count as f32 / files_count as f32,
                            speed: data.0.clone(),
                            time: data.1.clone(),
                            bytes_sent: data.2,
                        }))
                        .unwrap();

                        ctx.request_repaint();
                    }

                    if line.starts_with(|x| x == '>') || line.starts_with(|x| x == '<') {
                        count += 1;

                        tx.send(StateMessage::NextFile(NextFile {
                            line: line
                                .to_string()
                                .split(" ")
                                .last()
                                .unwrap_or_default()
                                .to_string(),
                        }))
                        .unwrap();

                        ctx.request_repaint();
                    }
                    println!("[rsync]: {}", line);
                }
            }

            buffer.clear();
        }

        tx.send(StateMessage::Finished(Default::default())).unwrap();
        ctx.request_repaint();
    });

    (rx, child)
}

fn parse_rsync_stats(lines: &String) -> HashMap<String, String> {
    let mut stats: HashMap<String, String> = HashMap::new();

    lazy_static! {
        static ref RE_KEY_VALUE: Regex = Regex::new(r"^(.+?):\s*(.*)$").unwrap();
        static ref RE_NUM_FILES: Regex = Regex::new(
            r"([\d.]+)\s+\(reg:\s*([\d.]+),\s*dir:\s*([\d.]+)(?:,\s*link:\s*([\d.]+))?\s*\)"
        )
        .unwrap();
        static ref RE_TOTAL_SPEEDUP: Regex =
            Regex::new(r"total size is ([\d.]+)\s+speedup is ([\d.,]+)\s+\((.*)\)").unwrap();
    }

    for line in lines.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() {
            continue;
        }

        if let Some(caps) = RE_KEY_VALUE.captures(trimmed_line) {
            let key = caps.get(1).unwrap().as_str().trim().to_string();
            let value = caps.get(2).unwrap().as_str().trim().to_string();

            if key == "Number of files" {
                if let Some(num_caps) = RE_NUM_FILES.captures(&value) {
                    stats.insert(
                        "Number of files (total)".to_string(),
                        num_caps
                            .get(1)
                            .map(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    );
                    stats.insert(
                        "Number of files (regular)".to_string(),
                        num_caps
                            .get(2)
                            .map(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    );
                    stats.insert(
                        "Number of files (directories)".to_string(),
                        num_caps
                            .get(3)
                            .map(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    );
                    stats.insert(
                        "Number of files (links)".to_string(),
                        num_caps
                            .get(4)
                            .map(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    );
                }
            } else {
                stats.insert(key, value);
            }
        } else if let Some(caps) = RE_TOTAL_SPEEDUP.captures(trimmed_line) {
            stats.insert(
                "Total size (summary)".to_string(),
                caps.get(1).unwrap().as_str().to_string(),
            );
            stats.insert(
                "Speedup".to_string(),
                caps.get(2).unwrap().as_str().to_string(),
            );
            stats.insert(
                "Run type".to_string(),
                caps.get(3).unwrap().as_str().to_string(),
            );
        }
    }

    stats
}

impl eframe::App for AppState {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.progress {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    StateMessage::Progress(x) => self.current_progress = x,
                    StateMessage::NextFile(x) => {
                        if !x.line.is_empty() {
                            self.logs.push_str(&x.line);
                            self.logs.push('\n');
                        }
                    }
                    StateMessage::Finished(_) => {
                        self.is_finished = true;
                        self.child = None;
                    }
                    StateMessage::Error(x) => {
                        self.error_logs.push_str(&x.line);
                        self.error_logs.push('\n');
                    }
                }
            }
        }

        ctx.set_pixels_per_point(1.2);
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("r-synced");
            if self.progress.is_some() {
                egui::Window::new("Operation Progress")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                    .show(ctx, |ui| {
                        ui.group(|ui| {
                            let progress_bar = ProgressBar::new(self.current_progress.total_progress)
                                .show_percentage()
                                .text(format!("{:.0}%", self.current_progress.total_progress * 100.0));
                            ui.add(progress_bar);

                            let progress_bar = ProgressBar::new(self.current_progress.progress)
                                .show_percentage()
                                .text(format!("{:.0}%", self.current_progress.progress * 100.0));
                            ui.add(progress_bar);

                            ui.label(format!("Speed: {} | Size: {} | ETA: {}", self.current_progress.speed, format_bytes(self.current_progress.bytes_sent), self.current_progress.time));

                            ui.group(|ui| {
                                ui.label("Logs");
                                ui.add_space(1f32);
                                egui::ScrollArea::vertical()
                                    .id_salt("logs_scrollarea")
                                    .stick_to_bottom(true)
                                    .auto_shrink([false; 2])
                                    .max_height(100.0)
                                    .show(ui, |ui| {
                                        ui.label(&self.logs);
                                    });
                            });


                            if !self.error_logs.is_empty() {
                                ui.group(|ui| {
                                    ui.label("Errors");
                                    ui.add_space(1f32);
                                    egui::ScrollArea::vertical()
                                        .id_salt("errors_scrollarea")
                                        .stick_to_bottom(true)
                                        .auto_shrink([false; 2])
                                        .max_height(100.0)
                                        .show(ui, |ui| {
                                            ui.label(&self.error_logs);
                                        });
                                });
                            }

                            if self.is_finished {
                                if ui.button("Continue").clicked() {
                                    self.progress = None
                                }
                            } else {
                                if ui.button("Cancel").clicked() {
                                    let pid = Pid::from_raw(self.child.as_ref().unwrap().id() as i32);
                                    if signal::kill(pid, Signal::SIGINT).is_ok() {
                                        self.logs.push_str("Operation Cancelled\n");
                                    }
                                }
                            }
                        });
                    });
            } else {
                ui.horizontal(|ui| {
                    ui.label("Source:");
                    ui.text_edit_singleline(&mut self.src);
                });

                ui.horizontal(|ui| {
                    ui.label("Destination:");
                    ui.text_edit_singleline(&mut self.dest);
                });

                let command = create_rsync_command(self);
                ui.group(|ui| {
                    ui.label("Command:");
                    ui.label(format!("{:?}", command));
                });

                ui.checkbox(&mut self.archive, "Archive (-a)");
                ui.add_enabled(!self.archive, Checkbox::new(&mut self.recursive, "Recursive (-r)"));
                ui.add_enabled(!self.archive, Checkbox::new(&mut self.symlinks, "Symlinks (-l)"));
                ui.add_enabled(!self.archive, Checkbox::new(&mut self.permissions, "Save Permissions (-p)"));
                ui.add_enabled(!self.archive, Checkbox::new(&mut self.time, "Save Modification Time (-t)"));
                ui.add_enabled(!self.archive, Checkbox::new(&mut self.group, "Save Group (-g)"));
                ui.checkbox(&mut self.compress, "Compress (-z)");

                if ui.button("Run").clicked() {
                    self.error_logs.clear();
                    self.logs.clear();
                    self.is_finished = false;
                    self.current_progress = Progress::default();

                    let mut dry_run = create_rsync_dry_run_command(self);
                    let output = dry_run.output().context("Failed to run dry-run").unwrap();
                    let result = String::from_utf8_lossy(&output.stdout).to_string();
                    let result_err = String::from_utf8_lossy(&output.stderr).to_string();

                    if !result_err.trim().is_empty() {
                        self.error_logs.push_str(&result_err);
                        self.error_logs.push('\n');
                        if result_err.contains("Permission denied") {
                            self.error_logs.push_str("Access denied when connecting to the server via SSH. Please check if your SSH key is configured.\n");
                            return;
                        }
                    }

                    let data = parse_rsync_stats(&result);
                    let number_of_files = data.get("Number of files (regular)");
                    if number_of_files.is_none() {
                        self.error_logs.push_str("Could not determine the file count for the transfer.\n");
                        self.error_logs.push_str(&result);
                        self.error_logs.push('\n');
                        return;
                    }

                    let command = create_rsync_command(self);
                    let rx = run_rsync(command, number_of_files.unwrap().replace(".", "").parse::<u64>().unwrap(), ctx.clone());
                    self.progress = Some(rx.0);
                    self.child = Some(rx.1);
                }

                if !self.error_logs.is_empty() {
                    ui.group(|ui| {
                        ui.label("Errors");
                        ui.add_space(1f32);
                        egui::ScrollArea::vertical()
                            .stick_to_bottom(true)
                            .auto_shrink([false; 2])
                            .max_height(100.0)
                            .show(ui, |ui| {
                                ui.label(&self.error_logs);
                            });
                    });
                }
            }
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([550.0, 550.0]),
        ..Default::default()
    };

    eframe::run_native(
        "r-synced",
        options,
        Box::new(|_cc| {
            Ok(Box::new(AppState::default()))
        }),
    )
}
