use anyhow::{Context, Result};
use self_update::backends::github::Update;
use std::{env, fs::File, path::PathBuf};

fn main() -> Result<()> {
	let out_path = PathBuf::from(env::var("OUT_DIR")?).join("Lemonade.rbxm");

	if !cfg!(feature = "plugin") {
		File::create(out_path)?;
		return Ok(());
	}

	let mut builder = Update::configure();

	if let Ok(token) = env::var("GITHUB_TOKEN") {
		builder.auth_token(&token);
	} else {
		println!("cargo:warning=GITHUB_TOKEN not set, rate limits may apply!")
	}

	builder
		.repo_owner("LupaHQ")
		.repo_name("argon-roblox")
		.bin_name("Lemonade.rbxm")
		.bin_install_path(out_path)
		.target("");

	builder
		.build()?
		.download()
		.context("Failed to download Lemonade plugin from GitHub!")?;

	Ok(())
}
