use anyhow::{bail, Result};
use log::{debug, info, trace, warn};
use self_update::{backends::github::Update, version::bump_is_greater, Status};
use serde::{Deserialize, Serialize};
use std::{
	fs,
	sync::Once,
	time::{Duration, SystemTime},
};
use yansi::Paint;

use crate::{
	argon_error, argon_info, argon_warn,
	constants::TEMPLATES_VERSION,
	installer::{get_plugin_version, install_templates},
	logger, sessions,
	util::{self, get_plugin_path},
};

static UPDATE_FORCED: Once = Once::new();
const UPDATE_CHECK_INTERVAL: u64 = 3600;

#[derive(Serialize, Deserialize)]
pub struct UpdateStatus {
	pub last_checked: SystemTime,
	pub plugin_version: String,
	pub templates_version: u8,
	pub vscode_version: String,
}

impl Default for UpdateStatus {
	fn default() -> Self {
		Self {
			last_checked: SystemTime::UNIX_EPOCH,
			plugin_version: String::new(),
			templates_version: TEMPLATES_VERSION,
			vscode_version: String::new(),
		}
	}
}

pub fn get_status() -> Result<UpdateStatus> {
	let path = util::get_argon_dir()?.join("update.toml");

	if path.exists() {
		match toml::from_str(&fs::read_to_string(&path)?) {
			Ok(status) => return Ok(status),
			Err(_) => warn!("Update status file is corrupted! Creating new one.."),
		}
	}

	// Try to get installed VS Code extension version
	let vscode_version = match get_vscode_version() {
		Some(version) => {
			trace!("Using detected VS Code extension version: {}", version);
			version
		}
		None => {
			trace!("Could not detect VS Code extension version, using default version");
			"0.0.0".to_string() // Use a baseline version if not detected
		}
	};

	let status = UpdateStatus {
		last_checked: SystemTime::UNIX_EPOCH,
		plugin_version: get_plugin_version(),
		templates_version: TEMPLATES_VERSION,
		vscode_version,
	};

	fs::write(path, toml::to_string(&status)?)?;

	Ok(status)
}

pub fn set_status(status: &UpdateStatus) -> Result<()> {
	let path = util::get_argon_dir()?.join("update.toml");

	fs::write(path, toml::to_string(status)?)?;

	Ok(())
}

async fn stop_running_sessions() -> Result<()> {
	let sessions = sessions::get_all()?;

	if sessions.is_empty() {
		debug!("No running sessions to stop before update");
		return Ok(());
	}

	info!("Stopping running sessions before update...");

	for (_, session) in sessions {
		if let Some(address) = session.get_address() {
			// Try to gracefully stop via HTTP first
			match reqwest::Client::new()
				.post(format!("{}/stop", address))
				.timeout(Duration::from_secs(5))
				.send()
				.await
			{
				Ok(_) => info!("Gracefully stopped session at {}", address),
				Err(e) => {
					warn!("Failed to gracefully stop session at {}: {}", address, e);
					// Fallback to force kill
					util::kill_process(session.pid);
					info!("Force stopped process with PID: {}", session.pid);
				}
			}
		} else {
			// No address, just kill the process
			util::kill_process(session.pid);
			info!("Stopped process with PID: {}", session.pid);
		}
	}

	// Clear all session records
	sessions::remove_all()?;
	info!("All sessions stopped successfully");

	Ok(())
}

pub fn update_cli(auto_update: bool) -> Result<bool> {
	let status = get_status()?;

	if status.last_checked.elapsed()?.as_secs() < UPDATE_CHECK_INTERVAL {
		debug!("Update was checked less than an hour ago, skipping..");
		return Ok(false);
	}

	let current_version = env!("CARGO_PKG_VERSION");
	let mut status = UpdateStatus {
		last_checked: SystemTime::now(),
		plugin_version: current_version.to_owned(),
		templates_version: TEMPLATES_VERSION,
		vscode_version: status.vscode_version,
	};

	let update = Update::configure()
		.repo_owner("LupaHQ")
		.repo_name("argon")
		.bin_name("argon")
		.current_version(current_version)
		.build()?;

	match update.get_latest_release() {
		Ok(release) => {
			if !bump_is_greater(&release.version, current_version)? {
				debug!("No new version available");
				set_status(&status)?;
				return Ok(false);
			}

			status.plugin_version = release.version.clone();

			if auto_update {
				info!("New version {} is available, updating..", release.version);

				// Stop running sessions before update
				tokio::runtime::Runtime::new()?.block_on(stop_running_sessions())?;

				match update.update()? {
					Status::Updated(version) => {
						argon_info!("Successfully updated to version {}!", version);
						set_status(&status)?;
						Ok(true)
					}
					Status::UpToDate(_) => {
						argon_warn!("Already using the latest version!");
						Ok(false)
					}
				}
			} else {
				argon_info!("New version {} is available! Run {}", release.version, "argon update");
				Ok(false)
			}
		}
		Err(err) => {
			argon_error!("Failed to check for updates: {}", err);
			bail!("Update check failed: {}", err);
		}
	}
}

fn update_plugin(status: &mut UpdateStatus, prompt: bool, force: bool) -> Result<bool> {
	let style = util::get_progress_style();
	let current_version = &status.plugin_version;
	let plugin_path = get_plugin_path()?;

	let update = Update::configure()
		.repo_owner("LupaHQ")
		.repo_name("argon-roblox")
		.bin_name("Argon.rbxm")
		.target("")
		.show_download_progress(true)
		.set_progress_style(style.0.clone(), style.1.clone())
		.bin_install_path(plugin_path)
		.build()?;

	let release = update.get_latest_release()?;

	if bump_is_greater(current_version, &release.version)? || force {
		if !prompt
			|| logger::prompt(
				&format!(
					"New version of Argon plugin: {} is available! Would you like to update?",
					release.version.bold()
				),
				true,
			) {
			if !prompt {
				argon_info!(
					"New version of Argon plugin: {} is available! Updating..",
					release.version.bold()
				);
			}

			match update.download() {
				Ok(_) => {
					argon_info!(
						"Roblox plugin updated! Make sure you have {} setting enabled to see changes. Visit {} to read the changelog",
						Paint::bold(&"Reload plugins on file changed"),
						Paint::bold(&"https://argon.wiki/changelog/argon-roblox")
					);

					status.plugin_version = release.version;
					Ok(true)
				}
				Err(err) => {
					println!("DEBUG: update_plugin failed: {}", err);
					argon_error!("Failed to update Argon plugin: {}", err);
					Ok(false)
				}
			}
		} else {
			trace!("Argon plugin is out of date!");
			Ok(false)
		}
	} else {
		trace!("Argon plugin is up to date!");
		Ok(false)
	}
}

fn update_templates(status: &mut UpdateStatus, prompt: bool, force: bool) -> Result<bool> {
	if status.templates_version < TEMPLATES_VERSION || force {
		if !prompt || logger::prompt("Default templates have changed! Would you like to update?", true) {
			if !prompt {
				argon_info!("Default templates have changed! Updating..",);
			}

			install_templates(true)?;

			status.templates_version = TEMPLATES_VERSION;

			Ok(true)
		} else {
			trace!("Templates are out of date!");
			Ok(false)
		}
	} else {
		trace!("Project templates are up to date!");
		Ok(false)
	}
}

// Get the currently installed VS Code extension version
fn get_vscode_version() -> Option<String> {
	// Try to get version using VS Code CLI
	let output = std::process::Command::new("code")
		.arg("--list-extensions")
		.arg("--show-versions")
		.output();

	match output {
		Ok(output) if output.status.success() => {
			let stdout = String::from_utf8_lossy(&output.stdout);
			trace!("VS Code extensions list: {}", stdout);
			// Look for the extension - might be "lemonade-labs.argon@x.y.z" or "argon@x.y.z"
			for line in stdout.lines() {
				if line.contains("lemonade-labs.argon@") || line.contains("argon@") {
					if let Some(version) = line.split('@').nth(1) {
						trace!("Found VS Code extension version: {}", version.trim());
						return Some(version.trim().to_string());
					}
				}
			}
			trace!("VS Code extension not found in installed extensions");
			None
		}
		Ok(output) => {
			trace!(
				"VS Code CLI returned error: {}",
				String::from_utf8_lossy(&output.stderr)
			);
			None
		}
		Err(err) => {
			trace!("Could not run VS Code CLI: {}", err);
			None
		}
	}
}

fn update_vscode(status: &mut UpdateStatus, prompt: bool, force: bool) -> Result<bool> {
	println!("DEBUG: Starting VS Code extension update process");
	trace!("Checking for VS Code extension updates");

	if let Some(current) = get_vscode_version() {
		println!("DEBUG: Current VS Code extension version detected: {}", current);
		trace!("Current VS Code extension version: {}", current);
		status.vscode_version = current;
	} else {
		println!(
			"DEBUG: Could not detect current VS Code extension version, using stored: {}",
			status.vscode_version
		);
		trace!(
			"Could not detect current VS Code extension version, using stored: {}",
			status.vscode_version
		);
	}

	let current_version = &status.vscode_version;
	println!("DEBUG: Current version to compare against: {}", current_version);

	println!("DEBUG: Fetching latest release from GitHub");
	trace!("Fetching latest VS Code extension release from GitHub");
	let client = reqwest::blocking::Client::builder().user_agent("argon-cli").build()?;

	let response = match client
		.get("https://api.github.com/repos/LupaHQ/argon-vscode/releases/latest")
		.send()
	{
		Ok(resp) => resp,
		Err(err) => {
			println!("DEBUG: Failed to send request for latest release: {}", err);
			trace!("Failed to get latest release information: {}", err);
			return Ok(false); // Early exit if network request fails
		}
	};

	if !response.status().is_success() {
		println!("DEBUG: GitHub API request failed with status: {}", response.status());
		trace!("GitHub API request failed with status: {}", response.status());
		return Ok(false); // Early exit on bad status
	}

	let release: serde_json::Value = match response.json() {
		Ok(json) => {
			println!("DEBUG: Successfully parsed GitHub API response");
			json
		}
		Err(err) => {
			println!("DEBUG: Failed to parse GitHub API response: {}", err);
			trace!("Failed to parse GitHub API response: {}", err);
			return Ok(false); // Early exit on parse error
		}
	};

	let latest_version_str = match release["tag_name"].as_str() {
		Some(tag) => tag.trim_start_matches('v').to_string(),
		None => {
			println!("DEBUG: Failed to get tag name from release");
			trace!("Failed to get tag name from release");
			return Ok(false);
		}
	};
	let latest_version = &latest_version_str; // Borrow for comparison

	println!(
		"DEBUG: Comparing versions - current: {}, latest: {}",
		current_version, latest_version
	);
	trace!("Latest VS Code extension version: {}", latest_version);

	let is_greater = bump_is_greater(current_version, latest_version);
	println!("DEBUG: Is latest version greater? {:?}", is_greater);

	let update_needed = match is_greater {
		Ok(result) => result || force,
		Err(err) => {
			println!("DEBUG: Failed to compare versions: {}", err);
			trace!("Failed to compare versions: {}", err);
			force // If comparison fails, only update if forced
		}
	};
	println!("DEBUG: Update needed? {} (force={})", update_needed, force);

	if update_needed {
		if !prompt
			|| logger::prompt(
				&format!(
					"New version of Argon VS Code extension: {} is available! Would you like to update?",
					Paint::bold(latest_version)
				),
				true,
			) {
			if !prompt {
				argon_info!(
					"New version of Argon VS Code extension: {} is available! Updating..",
					Paint::bold(latest_version)
				);
			}

			let assets = match release["assets"].as_array() {
				Some(assets) => assets,
				None => {
					trace!("Failed to get assets from release");
					return Ok(false);
				}
			};

			let vsix_asset = match assets.iter().find(|asset| {
				asset
					.get("name")
					.and_then(|n| n.as_str())
					.is_some_and(|name| name.ends_with(".vsix"))
			}) {
				Some(asset) => asset,
				None => {
					trace!("Failed to find VSIX asset in release");
					return Ok(false);
				}
			};

			let download_url = match vsix_asset.get("browser_download_url").and_then(|url| url.as_str()) {
				Some(url) => url.to_string(),
				None => {
					trace!("Failed to get download URL from asset");
					return Ok(false);
				}
			};

			let temp_dir = std::env::temp_dir();
			let vsix_path = temp_dir.join(format!("argon-{}.vsix", latest_version));

			if vsix_path.exists() {
				if let Err(err) = std::fs::remove_file(&vsix_path) {
					trace!("Failed to remove existing VSIX file: {}", err);
				}
			}

			argon_info!("Downloading VS Code extension...");
			println!("DEBUG: Downloading from URL: {}", download_url); // Debug URL
			trace!("Downloading from URL: {}", download_url);

			// Use a closure for download logic to handle intermediate errors cleanly
			let download_result = || -> Result<()> {
				let mut response = client.get(&download_url).send()?;
				if !response.status().is_success() {
					anyhow::bail!("Failed to download: HTTP status {}", response.status());
				}
				let mut file = std::fs::File::create(&vsix_path)?;
				std::io::copy(&mut response, &mut file)?;
				Ok(())
			};

			if let Err(err) = download_result() {
				println!("DEBUG: Download failed: {}", err);
				argon_error!("Failed to download VS Code extension: {}", err);
				return Ok(false);
			}

			argon_info!("Installing VS Code extension...");
			trace!("Running: code --install-extension {} --force", vsix_path.display());

			if !vsix_path.exists() {
				println!("DEBUG: VSIX file not found after download: {}", vsix_path.display());
				argon_error!("VSIX file not found at {}", vsix_path.display());
				return Ok(false);
			}

			let metadata = match std::fs::metadata(&vsix_path) {
				Ok(meta) => meta,
				Err(err) => {
					println!("DEBUG: Failed to get VSIX file metadata: {}", err);
					argon_error!("Failed to get VSIX file metadata: {}", err);
					return Ok(false);
				}
			};

			if metadata.len() == 0 {
				println!("DEBUG: Downloaded VSIX file is empty");
				argon_error!("Downloaded VSIX file is empty");
				return Ok(false);
			}

			let output = std::process::Command::new("code")
				.arg("--install-extension")
				.arg(&vsix_path)
				.arg("--force")
				.output();

			match output {
				Ok(output) => {
					let stdout = String::from_utf8_lossy(&output.stdout);
					let stderr = String::from_utf8_lossy(&output.stderr);
					println!("DEBUG: VS Code install stdout: {}", stdout);
					println!("DEBUG: VS Code install stderr: {}", stderr);
					trace!("VS Code stdout: {}", stdout);
					trace!("VS Code stderr: {}", stderr);

					if output.status.success() {
						let _ = std::fs::remove_file(vsix_path);
						argon_info!(
							"VS Code extension updated! Please reload VS Code to apply changes. Visit {} to read the changelog",
							Paint::bold(&"https://argon.wiki/changelog/argon-vscode")
						);
						status.vscode_version = latest_version_str; // Update status with owned String
						return Ok(true);
					} else {
						argon_error!("Failed to install VS Code extension: {}", stderr);
					}
				}
				Err(err) => {
					println!("DEBUG: Failed to run VS Code CLI: {}", err);
					argon_error!(
						"Failed to run VS Code CLI: {}. Ensure 'code' command is in your system PATH.",
						err
					);
				}
			}
		} else {
			println!("DEBUG: User declined VS Code update.");
			trace!("User declined update.");
		}
	} else {
		println!("DEBUG: VS Code extension already up to date.");
		trace!("Argon VS Code extension is up to date!");
	}

	Ok(false)
}

pub fn check_for_updates(plugin: bool, templates: bool, prompt: bool) -> Result<()> {
	let mut status = get_status()?;

	// If we've already checked within the last hour, skip
	let now = SystemTime::now();
	let one_hour = std::time::Duration::from_secs(60 * 60);
	if now.duration_since(status.last_checked).unwrap_or(one_hour) < one_hour {
		debug!("Update check already performed within the last hour");
		return Ok(());
	}

	update_cli(false)?;

	if plugin {
		update_plugin(&mut status, prompt, false)?;
	}

	if templates {
		update_templates(&mut status, prompt, false)?;
	}

	// Also check for VS Code extension updates
	let _ = update_vscode(&mut status, prompt, false);

	status.last_checked = SystemTime::now();
	set_status(&status)?;

	Ok(())
}

pub fn manual_update(cli: bool, plugin: bool, templates: bool, vscode: bool, force: bool) -> Result<bool> {
	UPDATE_FORCED.call_once(|| {});

	let mut status = get_status()?;
	let mut updated = false;

	// Update CLI first, as it might contain fixes for other update processes
	if cli {
		argon_info!("Checking for CLI updates...");
		if update_cli(force)? {
			updated = true;
		}
	}

	// Then update other components
	if plugin {
		argon_info!("Checking for Plugin updates...");
		if update_plugin(&mut status, false, force)? {
			updated = true;
		}
	}

	if templates {
		argon_info!("Checking for Template updates...");
		if update_templates(&mut status, false, force)? {
			updated = true;
		}
	}

	if vscode {
		argon_info!("Checking for VS Code extension updates...");
		if update_vscode(&mut status, false, force)? {
			updated = true;
		} else {
			trace!("No VS Code extension updates found or update failed");
		}
	}

	status.last_checked = SystemTime::now();
	set_status(&status)?;

	if !updated {
		argon_info!("All components are up to date!");
	}

	Ok(updated)
}
