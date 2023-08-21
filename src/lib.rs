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

use lazy_static::lazy_static;
use serde_derive::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

pub enum ExitCodes {
    Enoent = 2,     // No such file or directory
    Etime = 62,     // Timer expired
    SwUpdate = 100, // Software upgrade
}

pub mod host_insight {
    tonic::include_proto!("host_insight");
}

#[derive(Deserialize, Serialize)]
pub struct Identity {
    pub uid: String,
    pub domain: String,
}

#[derive(Deserialize)]
pub struct Config {
    pub can: Option<CanConfig>,
    pub digital_in: Option<DigitalInConfig>,
    pub digital_out: Option<DigitalOutConfig>,
    pub time: Time,
}

#[derive(Deserialize, Clone)]
pub struct DigitalInConfig {
    pub ports: Option<Vec<DigitalInPort>>,
}

#[derive(Deserialize, Clone)]
pub struct DigitalInPort {
    pub internal_name: String,
    pub external_name: String,
}

#[derive(Deserialize, Clone)]
pub struct DigitalOutConfig {
    pub ports: Option<Vec<DigitalOutPort>>,
}

#[derive(Deserialize, Clone)]
pub struct DigitalOutPort {
    pub internal_name: String,
    pub external_name: String,
    pub default_state: u8,
}

#[derive(Deserialize, Clone)]
pub struct CanConfig {
    pub ports: Option<Vec<CanPort>>,
    pub dbc_file: Option<String>,
}

#[derive(Deserialize, Clone)]
pub struct CanPort {
    pub name: String,
    pub bitrate: Option<u32>,
    pub listen_only: Option<bool>,
}

#[derive(Deserialize)]
pub struct Time {
    pub heartbeat_s: u64,
    pub sleep_max_s: u64,
    pub sleep_min_s: u64,
}

lazy_static! {
    pub static ref IDENTITY: Identity = load_identity();
    pub static ref CONFIG: Config = load_config();
}

pub const BIN_DIR: &str = env!("BIN_DIR");
pub const CONF_DIR: &str = env!("CONF_DIR");
pub const GIT_COMMIT_DESCRIBE: &str = env!("GIT_VERSION");

fn load_config() -> Config {
    let new_local_conf = PathBuf::from(format!("{}/conf-new.toml", CONF_DIR));
    let local_conf = PathBuf::from(format!("{}/conf.toml", CONF_DIR));
    let fallback_conf = PathBuf::from(format!("{}/conf-fallback.toml", CONF_DIR));

    if new_local_conf.exists() {
        if let Ok(s) = &fs::read_to_string(new_local_conf.clone()) {
            let result: Result<Config, toml::de::Error> = toml::from_str(s);
            if let Ok(config) = result {
                fs::rename(&new_local_conf, &local_conf).unwrap();
                return config;
            } else {
                eprintln!("The new local config is invalid. Removing it.");
                fs::remove_file(new_local_conf).unwrap();
            }
        } else {
            eprintln!("Could not parse the new local config as a string. Removing it...");
            fs::remove_file(new_local_conf).unwrap();
        };
    }
    toml::from_str(
        &fs::read_to_string(local_conf)
            .unwrap_or_else(|_| fs::read_to_string(fallback_conf).unwrap()),
    )
    .expect("Failed to load any config file.")
}

fn load_identity() -> Identity {
    let identity = PathBuf::from(format!("{}/identity.toml", CONF_DIR));
    let fallback_identity = PathBuf::from(format!("{}/identity-fallback.toml", CONF_DIR));

    toml::from_str(
        &fs::read_to_string(identity)
            .unwrap_or_else(|_| fs::read_to_string(fallback_identity).unwrap()),
    )
    .expect("Identity could not be established.")
}
