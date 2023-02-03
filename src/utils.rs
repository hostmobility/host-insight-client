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
use lib::{CONFIG, CONF_DIR, GIT_COMMIT_DESCRIBE};
use std::process::Command;

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

pub fn update_client(version: &str) -> Result<(), std::io::Error> {
    let mut process = Command::new("opkg")
        .arg("update")
        .spawn()
        .expect("Failed to execute opkg");

    match process.wait() {
        Ok(_) => {}
        Err(e) => {
            return Err(e);
        }
    };

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

    let current_package = if current_major < 2 {
        "host-insight-client"
    } else {
        "host-insight-client{current_major}"
    };

    if current_major < required_major {
        eprintln!("Removing current client package {current_package}...");
        process = Command::new("opkg")
            .arg("remove")
            .arg(current_package)
            .spawn()
            .expect("Failed to execute opkg");

        process.wait()?;

        let new_package = "host-insight-client{required_major}";
        let output = Command::new("opkg")
            .arg("install")
            .arg(new_package)
            .output()
            .expect("Failed to install {new_package}");

        if output.status.success() {
            eprintln!("Successfully installed {new_package}");
            Ok(())
        } else {
            eprintln!("Failed to install {new_package}.");
            eprintln!("Reinstalling {current_package}...");
            let output = Command::new("opkg")
                .arg("install")
                .arg(current_package)
                .output()
                .expect("Failed to install {current_package}");

            if output.status.success() {
                eprintln!("Successfully reinstalled {current_package}");
                Ok(())
            } else {
                eprintln!("Failed to reinstall {new_package}");
                let no_client_error =
                    std::io::Error::new(std::io::ErrorKind::Other, "No client installed!");
                Err(no_client_error)
            }
        }
    } else {
        process = Command::new("opkg")
            .arg("upgrade")
            .arg(current_package)
            .spawn()
            .expect("Failed to execute opkg");

        match process.wait() {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
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
