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

use super::gpio::{
    read_all_digital_in, send_value, REMOTE_CONTROL_BARRIER, REMOTE_CONTROL_IN_PROCESS,
};
use super::utils::{clean_up, fetch_resource, get_md5sum, update_client};
use async_std::task;
use lib::{
    host_insight::{agent_client::AgentClient, reply::Action, Reply, State},
    ExitCodes, Identity, CONFIG, CONF_DIR, GIT_COMMIT_DESCRIBE, IDENTITY,
};
use rand::Rng;
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use tonic::{
    transport::{Certificate, Channel, ClientTlsConfig},
    Request, Response, Status,
};

const SLEEP_OFFSET: f64 = 0.1;

pub async fn setup_network() -> Channel {
    // Connect to server
    let pem = tokio::fs::read("/etc/ssl/certs/ca-certificates.crt").await;
    let ca = Certificate::from_pem(pem.unwrap());

    let tls = ClientTlsConfig::new()
        .ca_certificate(ca)
        .domain_name(IDENTITY.domain.clone());

    let endpoint = Channel::builder(
        format!("https://{}", IDENTITY.domain.clone())
            .parse()
            .unwrap(),
    )
    .tls_config(tls)
    .unwrap();

    endpoint.connect_lazy()
}

pub async fn send_initial_values(channel: Channel) {
    let mut allow_remote_control = REMOTE_CONTROL_IN_PROCESS.lock().await;
    *allow_remote_control = true;
    drop(allow_remote_control);

    let initial_digital_in_vals: Option<HashMap<String, u8>> = read_all_digital_in().await;

    send_state(channel.clone()).await;

    if initial_digital_in_vals.is_some() {
        for (key, val) in initial_digital_in_vals.clone().unwrap() {
            send_value(channel.clone(), &key, val).await;
        }
    }
    let mut allow_remote_control = REMOTE_CONTROL_IN_PROCESS.lock().await;
    *allow_remote_control = false;
    drop(allow_remote_control);
}

pub async fn heartbeat(channel: Channel) -> Result<(), Box<dyn Error>> {
    let mut client = AgentClient::with_interceptor(channel, intercept);

    loop {
        let status = lib::host_insight::Status { code: 0 }; // Always report OK for now.
        task::sleep(Duration::from_secs(CONFIG.time.heartbeat_s)).await;
        let mut retry_sleep_s: u64 = CONFIG.time.sleep_min_s;

        loop {
            let response = client.heart_beat(status.clone()).await;
            if handle_send_result(response, &mut retry_sleep_s)
                .await
                .is_ok()
            {
                break;
            };
        }
    }
}

async fn send_state(channel: Channel) {
    let mut client = AgentClient::with_interceptor(channel, intercept);

    let local_conf = PathBuf::from(format!("{}/conf.toml", CONF_DIR));
    let fallback_conf = PathBuf::from(format!("{}/conf-fallback.toml", CONF_DIR));
    let current_config = if local_conf.exists() {
        local_conf
    } else if fallback_conf.exists() {
        fallback_conf
    } else {
        panic!("No config found");
    };

    let mut dbc_hash = None;
    if CONFIG.can.is_some() {
        let path = PathBuf::from(format!(
            "{}/{}",
            CONF_DIR,
            CONFIG.can.as_ref().unwrap().dbc_file.as_ref().unwrap()
        ));
        dbc_hash = get_md5sum(path.to_str().unwrap());
    };

    let config_hash = get_md5sum(current_config.to_str().unwrap());
    let state = State {
        sw_version: GIT_COMMIT_DESCRIBE.to_string(),
        config_md5sum: config_hash.unwrap(),
        dbc_md5sum: dbc_hash,
    };

    let mut retry_sleep_s: u64 = CONFIG.time.sleep_min_s;
    loop {
        let response = client.send_current_state(state.clone()).await;
        if handle_send_result(response, &mut retry_sleep_s)
            .await
            .is_ok()
        {
            break;
        };
    }
}

pub async fn handle_send_result(
    r: Result<Response<Reply>, Status>,
    s: &mut u64,
) -> Result<(), Status> {
    match r {
        Ok(r) => match r.into_inner().action {
            Some(Action::CarryOnMsg(_)) => {
                *s = CONFIG.time.sleep_min_s;
                return Ok(());
            }
            Some(Action::ExitMsg(msg)) => {
                clean_up();
                std::process::exit(msg.reason);
            }
            Some(Action::ControlRequestMsg(_)) => {
                *s = CONFIG.time.sleep_min_s;
                let allow_remote_control = REMOTE_CONTROL_IN_PROCESS.lock().await;
                if *allow_remote_control {
                    eprintln!("Remote control session is already in process.")
                } else {
                    REMOTE_CONTROL_BARRIER.wait().await;
                }
            }
            Some(Action::ConfigUpdateMsg(msg)) => {
                *s = CONFIG.time.sleep_min_s;
                println!("Config update");
                let new_local_conf = PathBuf::from(format!("{}/conf-new.toml", CONF_DIR));

                let mut file =
                    fs::File::create(new_local_conf).expect("Could not create new config file");
                file.write_all(&msg.config)
                    .expect("Failed to write new config file");

                clean_up();
                std::process::exit(0);
            }
            Some(Action::IdentityUpdateMsg(msg)) => {
                *s = CONFIG.time.sleep_min_s;
                println!("Identity update");
                let new_identity = Identity {
                    uid: msg.uid,
                    domain: msg.domain,
                };

                let toml_string =
                    toml::to_string(&new_identity).expect("Could not encode new identity as TOML");

                fs::write(
                    PathBuf::from(format!("{}/identity.toml", CONF_DIR)),
                    toml_string,
                )
                .expect("Could not write to file!");

                clean_up();
                std::process::exit(0);
            }
            Some(Action::FetchResourceMsg(msg)) => {
                *s = CONFIG.time.sleep_min_s;
                println!("Fetching resource");
                fetch_resource(&msg.url, msg.target_location)?;

                clean_up();
                std::process::exit(0);
            }
            Some(Action::SwUpdateMsg(msg)) => {
                *s = CONFIG.time.sleep_min_s;
                match update_client(&msg.version) {
                    Err(e) => eprintln!("{}: Failed to trigger software update.", e),
                    Ok(_) => {
                        clean_up();
                        std::process::exit(ExitCodes::SwUpdate as i32);
                    }
                };
            }
            _ => panic!("Unrecognized response"),
        },
        Err(e) => {
            eprintln!("Error: {e}");

            // Add a random sleep offset of +/- 10 % to avoid the
            // situation where all clients retry at the same time.
            // Make sure not to sleep any longer than max.
            let sleep = std::cmp::min(
                rand::thread_rng()
                    .gen_range(*s * (1.0 - SLEEP_OFFSET) as u64..=*s * (1.0 + SLEEP_OFFSET) as u64),
                CONFIG.time.sleep_max_s,
            );
            eprintln!("Sleeping for {sleep} s");
            task::sleep(Duration::from_secs(sleep)).await;

            if *s > CONFIG.time.sleep_max_s {
                eprintln!("Max sleep time reached");
                // Exit with code to let e.g. a systemd service handle this situation.
                std::process::exit(ExitCodes::Etime as i32);
            };

            // Double the sleep time to create a back-off effect.
            *s *= 2;

            return Err(e);
        }
    }
    Ok(())
}

pub fn intercept(mut req: Request<()>) -> Result<Request<()>, Status> {
    req.metadata_mut()
        .insert("uid", IDENTITY.uid.parse().unwrap());
    Ok(req)
}
