use anyhow::Result;
use clap::Parser;
use open;
use std::fs::File;

use crate::{argon_error, argon_info, config::Config as GlobalConfig, logger, util};

/// Edit global config with default editor
#[derive(Parser)]
pub struct Config {
	/// Setting to change (if left empty config will be opened)
	#[arg()]
	setting: Option<String>,

	/// Value to set setting to (if left empty default value will be used)
	#[arg()]
	value: Option<String>,

	/// List all available settings
	#[arg(short, long)]
	list: bool,
}

impl Config {
	pub fn main(self) -> Result<()> {
		if self.list {
			argon_info!("Available settings:\n");
			println!("{}", GlobalConfig::list());

			return Ok(());
		}

		match (self.setting, self.value) {
			(Some(setting), Some(value)) => {
				let mut config = GlobalConfig::load();

				if config.has_setting(&setting) {
					config.set(&setting, &value)?;

					config.save()?;
				} else {
					argon_error!("Setting '{}' does not exist", setting);
					return Ok(());
				}
			}
			(Some(setting), None) => {
				let default = GlobalConfig::load_default();

				if default.has_setting(&setting) {
					let mut config = GlobalConfig::load();

					config[&setting] = default[&setting].clone();

					config.save()?;
				} else {
					argon_error!("Setting '{}' does not exist", setting);
					return Ok(());
				}
			}
			_ => {
				let home_dir = util::get_home_dir()?;

				let config_path = home_dir.join(".argon").join("config.toml");

				if !config_path.exists() {
					let create_config = logger::prompt("Config does not exist. Would you like to create one?", true);

					if create_config {
						File::create(&config_path)?;
					} else {
						return Ok(());
					}
				}

				open::that(config_path)?;
			}
		}

		Ok(())
	}
}
