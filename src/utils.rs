// Copyright (C) 2023  Host Mobility AB

// This file is part of HOST Insight Client

// HOST Insight Client is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// HOST Insight Client is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program; if not, write to the Free Software Foundation,
// Inc., 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301  USA

use super::gpio::set_all_digital_out_to_defaults;
use anyhow::Error;
use lib::{CONFIG, CONF_DIR, GIT_COMMIT_DESCRIBE};
use std::fs;
use std::path::Path;
use std::process::Command;

static CLIENT_UPGRADE_PATH: &str = "/tmp/host-insight/client_upgrade";

pub fn fetch_resource(url: &str, dst: Option<String>) -> Result<(), std::io::Error> {
    if dst.is_some() {
        let mut process = Command::new("curl")
            .arg("-o")
            .arg(format!("{}/{}", CONF_DIR, dst.unwrap()))
            .arg(url)
            .spawn()
            .expect("Failed to execute curl.");
        process.wait()?;
    } else {
        let url_components: Vec<&str> = url.split('/').collect();
        let file_name = url_components[url_components.len() - 1];
        let mut process = Command::new("curl")
            .arg("-o")
            .arg(format!("{}/{}", CONF_DIR, file_name))
            .arg(url)
            .spawn()
            .expect("Failed to execute curl.");
        process.wait()?;
    }

    Ok(())
}

pub fn update_client(version: &str) -> Result<(), Error> {
    let current_version_components: Vec<&str> = GIT_COMMIT_DESCRIBE.split('.').collect();
    let required_version_components: Vec<&str> = version.split('.').collect();

    let current_major: u32 = current_version_components[0]
        .replace('v', "")
        .parse()
        .unwrap();
    let required_major: u32 = required_version_components[0]
        .replace('v', "")
        .parse()
        .unwrap();

    if current_major < required_major {
        // Write the requested upgrade to file for use by Host Insight helper
        if let Some(parent_dir) = Path::new(CLIENT_UPGRADE_PATH).parent() {
            fs::create_dir_all(parent_dir)?;
        }
        fs::write(CLIENT_UPGRADE_PATH, format!("{}", required_major))?;
        Ok(())
    } else {
        Err(Error::msg(
            "Required major version is not greater than the current major version.",
        ))
    }
}

pub fn clean_up() {
    if CONFIG.digital_out.is_some() {
        set_all_digital_out_to_defaults()
            .expect("Failed to set all digital outs to their default values.");
    }
}

// TODO: Make this function return Result<String, Error> Right now, it
// is Option<String> because dbc_hash can be None (if no dbc file
// exists).
pub fn get_md5sum(path: &str) -> Option<String> {
    let output = Command::new("md5sum").arg(path).output();

    match output {
        Ok(o) => Some(String::from_utf8(o.stdout).expect("Failed to parse stdout as utf8")),
        Err(_) => None,
    }
}
