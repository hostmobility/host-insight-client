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

use async_lock::Barrier;
use async_std::{sync::Mutex, task};
use can_dbc::{ByteOrder, MultiplexIndicator, SignalExtendedValueType};
use clap::command;
use futures::future::try_join_all;
use futures::stream;
use futures::stream::StreamExt;
use gpio_cdev::{AsyncLineEventHandle, Chip, EventRequestFlags, EventType, LineRequestFlags};
use host_insight::agent_client::AgentClient;
use host_insight::remote_control_client::RemoteControlClient;
use host_insight::{
    can_signal, reply::Action, CanMessage, CanSignal, ControlStatus, GpioState, State,
    UnitControlStatus, Value, Values,
};
use lazy_static::lazy_static;
use rand::Rng;
use serde_derive::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fs;
use std::io::prelude::*;
use std::path::PathBuf;
use std::str;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tokio_socketcan::CANSocket;
use tonic::{
    transport::{Certificate, Channel, ClientTlsConfig},
    Request, Response, Status,
};

pub mod host_insight {
    tonic::include_proto!("host_insight");
}

lazy_static! {
    static ref CONFIG: Config = load_config();
    static ref IDENTITY: Identity = load_identity();
}

lazy_static! {
    static ref DIGITAL_OUT_MAP: Option<HashMap<String, DigitalOutPort>> = create_digital_out_map();
}

lazy_static! {
    static ref CAN_MSG_QUEUE: Mutex<Vec<CanMessage>> = Mutex::new(Vec::new());
}

lazy_static! {
    static ref REMOTE_CONTROL_BARRIER: Arc<Barrier> = Arc::new(Barrier::new(2));
    static ref REMOTE_CONTROL_IN_PROCESS: Mutex<bool> = Mutex::new(false);
}

pub const GIT_COMMIT_DESCRIBE: &str = env!("GIT_VERSION");
pub const BIN_DIR: &str = env!("BIN_DIR");
pub const CONF_DIR: &str = env!("CONF_DIR");

const SLEEP_OFFSET: f64 = 0.1;

enum ErrorCodes {
    Etime = 62, // Timer expired
}

#[derive(Deserialize, Serialize)]
struct Identity {
    uid: String,
    domain: String,
}

#[derive(Deserialize)]
struct Config {
    can: Option<CanConfig>,
    digital_in: Option<DigitalInConfig>,
    digital_out: Option<DigitalOutConfig>,
    time: Time,
}

#[derive(Deserialize, Clone)]
struct DigitalInConfig {
    ports: Option<Vec<DigitalInPort>>,
}

#[derive(Deserialize, Clone)]
struct DigitalInPort {
    internal_name: String,
    external_name: String,
}

#[derive(Deserialize, Clone)]
struct DigitalOutConfig {
    ports: Option<Vec<DigitalOutPort>>,
}

#[derive(Deserialize, Clone)]
struct DigitalOutPort {
    internal_name: String,
    external_name: String,
    default_state: u8,
}

#[derive(Deserialize, Clone)]
struct CanConfig {
    ports: Option<Vec<CanPort>>,
    dbc_file: Option<String>,
}

#[derive(Deserialize, Clone)]
struct CanPort {
    name: String,
    bitrate: Option<u32>,
    listen_only: Option<bool>,
}

#[derive(Deserialize)]
struct Time {
    heartbeat_s: u64,
    sleep_max_s: u64,
    sleep_min_s: u64,
}

fn intercept(mut req: Request<()>) -> Result<Request<()>, Status> {
    req.metadata_mut()
        .insert("uid", IDENTITY.uid.parse().unwrap());
    Ok(req)
}

fn clean_up() {
    if CONFIG.digital_out.is_some() {
        set_all_digital_out_to_defaults()
            .expect("Failed to set all digital outs to their default values.");
    }
}

async fn handle_send_result(
    r: Result<Response<host_insight::Reply>, Status>,
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
                    Err(e) => panic!("Error: {e}"),
                    Ok(_) => {
                        clean_up();
                        std::process::exit(0);
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
                std::process::exit(ErrorCodes::Etime as i32);
            };

            // Double the sleep time to create a back-off effect.
            *s *= 2;

            return Err(e);
        }
    }
    Ok(())
}

async fn send_value(channel: Channel, channel_name: &str, channel_vale: u8) {
    let mut client = AgentClient::with_interceptor(channel, intercept);

    //Create Vector "list" of Value. Value is defined in host_insight.proto
    let mut v: Vec<Value> = Vec::new();

    //Create measurement of type Value
    let meas = Value {
        name: channel_name.into(),
        value: channel_vale as i32,
    };
    //Add measurement to vector "list"
    v.push(meas);

    let mut retry_sleep_s: u64 = CONFIG.time.sleep_min_s;
    loop {
        //Create request of type Values. Values is defined in host_insight.proto
        let request = Request::new(Values {
            measurements: v.clone(),
        });

        //Send values. send_values is autogenerated when host_insight.proto is compiled
        //send_values is the defined RPC SendValues. Rust converts to snake_case
        let response = client.send_values(request).await;
        if handle_send_result(response, &mut retry_sleep_s)
            .await
            .is_ok()
        {
            break;
        };
    }
}

async fn send_can_message_stream(channel: Channel, can_messages: Vec<CanMessage>) {
    let mut client = AgentClient::with_interceptor(channel, intercept);

    let mut retry_sleep_s: u64 = CONFIG.time.sleep_min_s;
    loop {
        //Create request of type CanMessage. The latter is defined in host_insight.proto
        let request = Request::new(stream::iter(can_messages.clone()));

        let response = client.send_can_message_stream(request).await;
        if handle_send_result(response, &mut retry_sleep_s)
            .await
            .is_ok()
        {
            break;
        };
    }
}

fn fetch_resource(url: &str, dst: Option<String>) -> Result<(), std::io::Error> {
    if dst.is_some() {
        let mut process = std::process::Command::new("curl")
            .arg("-o")
            .arg(format!("{}/{}", CONF_DIR, dst.unwrap()))
            .arg(url)
            .spawn()
            .expect("Failed to execute curl.");
        process.wait()?;
    } else {
        let url_components: Vec<&str> = url.split('/').collect();
        let file_name = url_components[url_components.len() - 1];
        let mut process = std::process::Command::new("curl")
            .arg("-o")
            .arg(format!("{}/{}", CONF_DIR, file_name))
            .arg(url)
            .spawn()
            .expect("Failed to execute curl.");
        process.wait()?;
    }

    Ok(())
}

// TODO: Make this function return Result<String, Error> Right now, it
// is Option<String> because dbc_hash can be None (if no dbc file
// exists).
fn get_md5sum(path: &str) -> Option<String> {
    let output = std::process::Command::new("md5sum").arg(path).output();

    match output {
        Ok(o) => Some(String::from_utf8(o.stdout).expect("Failed to parse stdout as utf8")),
        Err(_) => None,
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

#[allow(dead_code)]
async fn send_can_message(channel: Channel, can_message: CanMessage) {
    let mut client = AgentClient::with_interceptor(channel, intercept);

    let mut retry_sleep_s: u64 = CONFIG.time.sleep_min_s;
    loop {
        let request = Request::new(can_message.clone());
        let response = client.send_can_message(request).await;
        if handle_send_result(response, &mut retry_sleep_s)
            .await
            .is_ok()
        {
            break;
        }
    }
}

fn load_dbc_file(s: &str) -> Result<can_dbc::DBC, Box<dyn Error>> {
    let path = PathBuf::from(format!("{}/{}", CONF_DIR, s));
    let mut f = fs::File::open(path)?;
    let mut buffer = Vec::new();
    f.read_to_end(&mut buffer)?;
    let dbc = can_dbc::DBC::from_slice(&buffer).expect("Failed to parse dbc file");
    Ok(dbc)
}

// Checks if the last signal value sent is equal to supllied signal and value
fn is_can_signal_duplicate(
    map: &HashMap<String, Option<can_signal::Value>>,
    name: &str,
    val: &Option<can_signal::Value>,
) -> bool {
    if let Some(last_sent) = map.get_key_value(name) {
        if Some(last_sent.1) == Some(val) {
            return true;
        }
    }
    false
}

async fn can_sender(channel: Channel) -> Result<i32, Box<dyn Error>> {
    const MAX_MSG_TO_SEND: usize = 100;

    loop {
        let mut vec = Vec::new();

        let mut req_map = CAN_MSG_QUEUE.lock().await;

        let len = req_map.len();

        if len == 0 {
            drop(req_map);
            sleep(Duration::from_millis(100)).await;
            continue;
        } else {
            if len > MAX_MSG_TO_SEND {
                vec.extend(req_map.drain(..MAX_MSG_TO_SEND));
            } else {
                vec.extend(req_map.drain(..));
            }
            drop(req_map);
        }

        send_can_message_stream(channel.clone(), vec).await;
    }
}

async fn remote_control_monitor(channel: Channel) -> Result<(), Box<dyn Error>> {
    let mut client = RemoteControlClient::with_interceptor(channel, intercept);
    let status = ControlStatus {
        code: UnitControlStatus::UnitReady as i32,
    };
    loop {
        REMOTE_CONTROL_BARRIER.wait().await;
        let mut allow_remote_control = REMOTE_CONTROL_IN_PROCESS.lock().await;
        *allow_remote_control = true;
        drop(allow_remote_control);
        let mut stream = client
            .control_stream(status.clone())
            .await
            .unwrap()
            .into_inner();
        while let Some(item) = stream.next().await {
            match item.as_ref() {
                Err(e) => {
                    eprintln!("Error: Item from remote control stream did not contain a command.");
                    eprintln!("{e}");
                    set_all_digital_out_to_defaults()?;
                    let mut allow_remote_control = REMOTE_CONTROL_IN_PROCESS.lock().await;
                    *allow_remote_control = false;
                    drop(allow_remote_control);
                    break;
                }
                Ok(item) => {
                    if item.cmd == "Close" {
                        set_all_digital_out_to_defaults()?;
                        let mut allow_remote_control = REMOTE_CONTROL_IN_PROCESS.lock().await;
                        *allow_remote_control = false;
                        drop(allow_remote_control);
                        break;
                    } else if !DIGITAL_OUT_MAP.as_ref().unwrap().contains_key(&item.cmd) {
                        eprintln!("Invalid command: {}.", &item.cmd);
                    } else {
                        set_digital_out(&item.cmd, item.state)?;
                    }
                }
            };
        }
    }
}

async fn can_monitor(port: &CanPort) -> Result<(), Box<dyn Error>> {
    let dbc = load_dbc_file(CONFIG.can.as_ref().unwrap().dbc_file.as_ref().unwrap())
        .expect("Failed to load DBC file");

    let mut map = HashMap::new();
    let mut prev_map = HashMap::new();
    for message in dbc.messages() {
        map.insert(message.message_id().0, message);
    }

    let mut msg_map = HashMap::new();
    for message in dbc.messages() {
        msg_map.insert(message.message_id().0, message);
    }

    let mut socket_rx = CANSocket::open(&port.name.clone())?;
    eprintln!("Start reading from {}", &port.name);
    if let Some(bitrate) = &port.bitrate {
        eprintln!("Bitrate: {bitrate}");
    }

    while let Some(frame) = socket_rx.next().await {
        if let Some(message) = msg_map.get_key_value(&frame.as_ref().unwrap().id()) {
            if frame.as_ref().unwrap().id() == message.1.message_id().0 {
                let data = frame.as_ref().unwrap().data();
                let mut can_signals: Vec<CanSignal> = Vec::new();

                let mut multiplex_val = 0;

                for signal in message.1.signals() {
                    let can_signal_value =
                        match get_can_signal_value(message.1.message_id(), data, signal, &dbc) {
                            Some(val) => Some(val),
                            // FIXME: Report an error to the server instead of just skipping the signal
                            None => continue,
                        };

                    let signal_unit = if str::is_empty(signal.unit()) {
                        match can_signal_value {
                            Some(host_insight::can_signal::Value::ValStr(_)) => "enum".to_string(),
                            _ => "N/A".to_string(),
                        }
                    } else {
                        signal.unit().clone()
                    };
                    // If the signal is a multiplexor, store the value of that signal.
                    if is_multiplexor(signal) {
                        if let Some(can_signal::Value::ValU64(val)) = can_signal_value.clone() {
                            multiplex_val = val;
                        }
                        continue;
                    }

                    // If the value is a multiplexed signal
                    // Check if the multiplex signal value matches the multiplexor value of this signal
                    // Else continue and discard the signal
                    // FIXME: This is dependent on that the multipexor signal is parsed firs in the for-loop.
                    // otherwise the multiplex_val variable will be 0
                    if is_multiplexed(signal) {
                        if let Some(can_signal::Value::ValU64(_)) = can_signal_value.clone() {
                            if multiplex_val != get_multiplex_val(signal) {
                                continue;
                            }
                        }
                    }

                    let can_signal: CanSignal = CanSignal {
                        signal_name: signal.name().clone(),
                        unit: signal_unit,
                        value: can_signal_value.clone(),
                    };
                    if is_can_signal_duplicate(&prev_map, signal.name(), &can_signal_value) {
                        continue;
                    }
                    *prev_map
                        .entry(signal.name().clone())
                        .or_insert_with(|| can_signal_value.clone()) = can_signal_value.clone();
                    can_signals.push(can_signal);
                }

                if can_signals.is_empty() {
                    continue;
                }

                let can_message: CanMessage = CanMessage {
                    bus: port.name.clone(),
                    time_stamp: None, // The tokio_socketcan library currently lacks support for timestamps, but see https://github.com/socketcan-rs/socketcan-rs/issues/22
                    signal: can_signals.clone(),
                };
                let mut req_map = CAN_MSG_QUEUE.lock().await;

                req_map.push(can_message);
            }
        }
    }
    Ok(())
}

async fn digital_in_monitor(port: &DigitalInPort, channel: Channel) -> Result<(), Box<dyn Error>> {
    if let Some((chip_name, line_number)) = get_digital_chip_and_line(&port.internal_name) {
        let mut chip = Chip::new(chip_name)?;
        let line = chip.get_line(line_number)?;

        let mut events = AsyncLineEventHandle::new(line.events(
            LineRequestFlags::INPUT,
            EventRequestFlags::BOTH_EDGES,
            "gpioevents",
        )?)?;

        while let Some(event) = events.next().await {
            send_value(
                channel.clone(),
                &port.external_name,
                (event?.event_type() == EventType::RisingEdge) as u8,
            )
            .await
        }
        Ok(())
    } else {
        Err("Could not find chip name or line number from {&port.internal}".into())
    }
}

async fn send_initial_values(
    channel: Channel,
    initial_digital_in_vals: Option<HashMap<String, u8>>,
) {
    let mut allow_remote_control = REMOTE_CONTROL_IN_PROCESS.lock().await;
    *allow_remote_control = true;
    drop(allow_remote_control);

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

// Create a HashMap<external name, port> for digital outs
fn create_digital_out_map() -> Option<HashMap<String, DigitalOutPort>> {
    if CONFIG.digital_out.is_some() {
        let mut map: HashMap<String, DigitalOutPort> = HashMap::new();
        let ports = CONFIG.digital_out.clone().unwrap().ports.unwrap();
        for p in ports {
            map.insert(p.external_name.clone(), p);
        }
        return Some(map);
    }
    None
}

fn set_digital_out(external_name: &str, state: i32) -> Result<(), gpio_cdev::Error> {
    let p = DIGITAL_OUT_MAP
        .as_ref()
        .expect("Could not find digital out map.")
        .get(external_name)
        .expect("Could not map external name to port.");
    let internal_name = &p.internal_name;

    if let Some((chip_name, line)) = get_digital_chip_and_line(internal_name) {
        if let Ok(mut chip) = Chip::new(chip_name) {
            let handle = chip
                .get_line(line)
                .unwrap()
                .request(
                    LineRequestFlags::OUTPUT,
                    0,
                    "set_digital_out {external_name} to {state}",
                )
                .unwrap();

            if state == GpioState::Active as i32 {
                handle.set_value(1 - p.default_state)?;
            } else {
                handle.set_value(p.default_state)?;
            }
        }
    }
    Ok(())
}

fn set_all_digital_out_to_defaults() -> Result<(), gpio_cdev::Error> {
    for (i, p) in CONFIG.digital_out.clone().unwrap().ports.iter().enumerate() {
        if let Some((chip_name, line)) = get_digital_chip_and_line(&p[i].internal_name) {
            if let Ok(mut chip) = Chip::new(chip_name) {
                let handle = chip
                    .get_line(line)
                    .unwrap()
                    .request(
                        LineRequestFlags::OUTPUT,
                        0,
                        "set_all_digital_out_to_defaults",
                    )
                    .unwrap();

                handle.set_value(p[i].default_state)?;
            }
        }
    }
    Ok(())
}

fn setup_can() {
    for p in CONFIG.can.clone().unwrap().ports.unwrap() {
        let interface = p.name;

        let mut bitrate = "500000".to_string();
        if p.bitrate.is_some() {
            bitrate = p.bitrate.unwrap().to_string();
        }

        // ip link set INTERFACE down
        let mut process = std::process::Command::new("ip")
            .arg("link")
            .arg("set")
            .arg(&interface)
            .arg("down")
            .spawn()
            .expect("Failed to run ip command.");
        match process.wait() {
            Ok(_) => eprintln!("Interface {} is down", &interface),
            Err(e) => panic!("Error: {}", e),
        }

        // ip link set up INTERFACE type can bitrate BITRATE listen-only {ON/OFF}
        let mut listen_only_state = "on";
        if p.listen_only.is_some() && !p.listen_only.unwrap() {
            listen_only_state = "off";
        }
        let mut process = std::process::Command::new("ip")
            .arg("link")
            .arg("set")
            .arg("up")
            .arg(&interface)
            .arg("type")
            .arg("can")
            .arg("bitrate")
            .arg(&bitrate)
            .arg("listen-only")
            .arg(listen_only_state)
            .spawn()
            .expect("Failed to run ip command.");
        match process.wait() {
            Ok(_) => eprintln!("Interface {} is up", &interface),
            Err(e) => panic!("Error: {}", e),
        }
    }
}

async fn setup_server() -> Channel {
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

fn load_identity() -> Identity {
    let identity = PathBuf::from(format!("{}/identity.toml", CONF_DIR));
    let fallback_identity = PathBuf::from(format!("{}/identity-fallback.toml", CONF_DIR));

    toml::from_str(
        &fs::read_to_string(identity)
            .unwrap_or_else(|_| fs::read_to_string(fallback_identity).unwrap()),
    )
    .expect("Identity could not be established.")
}

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

// Update list of packages and then upgrade client to the required version
fn update_client(version: &str) -> Result<(), std::io::Error> {
    let mut process = std::process::Command::new("opkg")
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
        process = std::process::Command::new("opkg")
            .arg("remove")
            .arg(current_package)
            .spawn()
            .expect("Failed to execute opkg");

        process.wait()?;

        let new_package = "host-insight-client{required_major}";
        let output = std::process::Command::new("opkg")
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
            let output = std::process::Command::new("opkg")
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
        process = std::process::Command::new("opkg")
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    command!().version(GIT_COMMIT_DESCRIBE).get_matches();

    println!("Starting HOST Insight Client {}", GIT_COMMIT_DESCRIBE);
    let channel = setup_server().await;

    if CONFIG.digital_out.is_some() {
        set_all_digital_out_to_defaults()?;
    }

    // Get and send initial Digital IN values
    let initial_digital_in_vals: Option<HashMap<String, u8>> = read_all_digital_in().await;
    send_initial_values(channel.clone(), initial_digital_in_vals).await;

    let heartbeat_future = heartbeat(channel.clone());
    let remote_control_future = remote_control_monitor(channel.clone());

    // TODO: refactor this ugly part
    if CONFIG.digital_in.is_some() && CONFIG.can.is_some() {
        let digital_in_ports = CONFIG.digital_in.clone().unwrap().ports.unwrap();
        let mut digital_in_monitor_futures =
            vec![digital_in_monitor(&digital_in_ports[0], channel.clone())];
        for p in &digital_in_ports[1..] {
            digital_in_monitor_futures.push(digital_in_monitor(p, channel.clone()));
        }

        setup_can();
        let can_ports = CONFIG.can.clone().unwrap().ports.unwrap();
        let mut can_monitor_futures = vec![can_monitor(&can_ports[0])];
        for p in &can_ports[1..] {
            can_monitor_futures.push(can_monitor(p));
        }
        let sender_handle = can_sender(channel);
        match tokio::try_join!(
            try_join_all(digital_in_monitor_futures),
            try_join_all(can_monitor_futures),
            remote_control_future,
            heartbeat_future,
            sender_handle,
        ) {
            Ok(_) => eprintln!("All tasks completed successfully"),
            Err(e) => eprintln!("Some task failed: {e}"),
        };
    } else if CONFIG.can.is_some() {
        setup_can();
        let sender_handle = can_sender(channel);
        let can_ports = CONFIG.can.clone().unwrap().ports.unwrap();
        let mut can_monitor_futures = vec![can_monitor(&can_ports[0])];
        for p in &can_ports[1..] {
            can_monitor_futures.push(can_monitor(p));
        }
        match tokio::try_join!(
            try_join_all(can_monitor_futures),
            remote_control_future,
            heartbeat_future,
            sender_handle,
        ) {
            Ok(_) => eprintln!("All tasks completed successfully"),
            Err(e) => eprintln!("Some task failed: {e}"),
        };
    } else if CONFIG.digital_in.is_some() {
        let digital_in_ports = CONFIG.digital_in.clone().unwrap().ports.unwrap();
        let mut digital_in_monitor_futures =
            vec![digital_in_monitor(&digital_in_ports[0], channel.clone())];
        for p in &digital_in_ports[1..] {
            digital_in_monitor_futures.push(digital_in_monitor(p, channel.clone()));
        }

        match tokio::try_join!(
            try_join_all(digital_in_monitor_futures),
            heartbeat_future,
            remote_control_future,
        ) {
            Ok(_) => eprintln!("All tasks completed successfully"),
            Err(e) => eprintln!("Some task failed: {e}"),
        };
    } else {
        match tokio::try_join!(heartbeat_future, remote_control_future) {
            Ok(_) => eprintln!("All tasks completed successfully"),
            Err(e) => eprintln!("Some task failed: {e}"),
        };
    }

    clean_up();
    Ok(())
}

async fn heartbeat(channel: Channel) -> Result<(), Box<dyn Error>> {
    let mut client = AgentClient::with_interceptor(channel, intercept);

    loop {
        let status = host_insight::Status { code: 0 }; // Always report OK for now.
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

fn get_digital_chip_and_line(internal_port_name: &str) -> Option<(String, u32)> {
    let chip_iterator = match gpio_cdev::chips() {
        Ok(chips) => chips,
        Err(e) => {
            eprintln!("Failed to get chip iterator: {:?}", e);
            return None;
        }
    };

    for chip in chip_iterator.flatten() {
        for line in chip.lines() {
            match line.info() {
                Ok(info) => {
                    if info.name().unwrap_or("unused") == internal_port_name {
                        let c = format!("/dev/{}", chip.name());
                        let l: u32 = info.line().offset();
                        return Some((c, l));
                    }
                }
                _ => return None,
            }
        }
    }
    None
}

// Get some HashMap of <external name, value> or None
async fn read_all_digital_in() -> Option<HashMap<String, u8>> {
    let mut external_name_values = HashMap::new();

    for (i, p) in CONFIG.digital_in.as_ref()?.clone().ports.iter().enumerate() {
        if let Some((chip_name, line)) = get_digital_chip_and_line(&p[i].internal_name) {
            if let Ok(mut chip) = Chip::new(chip_name) {
                let handle = chip
                    .get_line(line)
                    .unwrap()
                    .request(LineRequestFlags::INPUT, 0, "read-input")
                    .unwrap();
                external_name_values
                    .insert(p[i].external_name.clone(), handle.get_value().unwrap());
            }
        }
    }

    if external_name_values.is_empty() {
        None
    } else {
        Some(external_name_values)
    }
}

// Get the can signal value based on the message ID, the data part of
// the frame, the signal, and extra metadata contained in the DBC
// file.
// The following can_signal::Value types can be returned:
//   Value::ValF64, ValStr, ValI64, ValU64
fn get_can_signal_value(
    id: &can_dbc::MessageId,
    d: &[u8],
    s: &can_dbc::Signal,
    dbc: &can_dbc::DBC,
) -> Option<host_insight::can_signal::Value> {
    let mut frame_data: [u8; 8] = [0; 8];
    if *s.byte_order() == ByteOrder::LittleEndian {
        for (index, value) in d.iter().enumerate() {
            frame_data[index] = *value;
        }
    }

    let frame_value: u64 = if *s.byte_order() == ByteOrder::LittleEndian {
        u64::from_le_bytes(frame_data)
    } else {
        u64::from_be_bytes(frame_data)
    };

    let signal_value = get_signal_value(frame_value, *s.start_bit(), *s.signal_size());

    match get_signal_value_type(s, dbc, id) {
        Some(SignalValueType::Float) => get_float(signal_value, *s.factor(), *s.offset()),
        Some(SignalValueType::Signed) => {
            get_signed_number(signal_value, *s.signal_size(), *s.factor(), *s.offset())
        }
        Some(SignalValueType::Unsigned) => {
            get_unsigned_number(signal_value, *s.factor(), *s.offset())
        }
        Some(SignalValueType::Double) => get_double(signal_value, *s.factor(), *s.offset()),
        // FIXME: IMPLEMENT BOOL
        Some(SignalValueType::String) => get_string(signal_value, dbc, id, s),
        _ => None,
    }
}

fn is_multiplexor(s: &can_dbc::Signal) -> bool {
    match s.multiplexer_indicator() {
        MultiplexIndicator::Multiplexor => true,
        MultiplexIndicator::MultiplexedSignal(_val) => false,
        MultiplexIndicator::MultiplexorAndMultiplexedSignal(_val) => false,
        MultiplexIndicator::Plain => false,
    }
}

fn is_multiplexed(s: &can_dbc::Signal) -> bool {
    match s.multiplexer_indicator() {
        MultiplexIndicator::Multiplexor => false,
        MultiplexIndicator::MultiplexedSignal(_val) => true,
        MultiplexIndicator::MultiplexorAndMultiplexedSignal(_val) => false,
        MultiplexIndicator::Plain => false,
    }
}

fn get_multiplex_val(s: &can_dbc::Signal) -> u64 {
    match s.multiplexer_indicator() {
        MultiplexIndicator::Multiplexor => 0,
        MultiplexIndicator::MultiplexedSignal(val) => *val,
        MultiplexIndicator::MultiplexorAndMultiplexedSignal(val) => *val,
        MultiplexIndicator::Plain => 0,
    }
}

#[derive(Debug)]
enum SignalValueType {
    Float,
    Signed,
    Unsigned,
    Double,
    // Bool,  UNIMPLEMENTED
    String,
}

fn get_signal_value_type(
    s: &can_dbc::Signal,
    dbc: &can_dbc::DBC,
    id: &can_dbc::MessageId,
) -> Option<SignalValueType> {
    let val_desc = dbc.value_descriptions_for_signal(*id, s.name());
    if val_desc.is_some() {
        return Some(SignalValueType::String);
    }

    let mut value_type_extended: Option<can_dbc::SignalExtendedValueType> =
        Some(can_dbc::SignalExtendedValueType::SignedOrUnsignedInteger);

    for elem in dbc.signal_extended_value_type_list() {
        if elem.signal_name() == s.name() {
            value_type_extended = Some(*elem.signal_extended_value_type());
            break;
        }
    }
    match value_type_extended {
        Some(SignalExtendedValueType::IEEEfloat32Bit) => Some(SignalValueType::Float),
        Some(SignalExtendedValueType::IEEEdouble64bit) => Some(SignalValueType::Double),
        Some(SignalExtendedValueType::SignedOrUnsignedInteger) => match *s.value_type() {
            can_dbc::ValueType::Unsigned => Some(SignalValueType::Unsigned),
            can_dbc::ValueType::Signed => Some(SignalValueType::Signed),
        },
        _ => None,
    }
}

fn get_string(
    signal_value: u64,
    dbc: &can_dbc::DBC,
    id: &can_dbc::MessageId,
    s: &can_dbc::Signal,
) -> Option<host_insight::can_signal::Value> {
    let val_desc = dbc.value_descriptions_for_signal(*id, s.name());

    if let Some(desc) = val_desc {
        for elem in desc {
            if *elem.a() == signal_value as f64 {
                return Some(host_insight::can_signal::Value::ValStr(
                    elem.b().to_string(),
                ));
            }
        }
        // Signal exists in value description but key could not be found
        return Some(host_insight::can_signal::Value::ValStr(
            signal_value.to_string(),
        ));
    }
    None
}

fn get_float(
    signal_value: u64,
    signal_factor: f64,
    signal_offset: f64,
) -> Option<host_insight::can_signal::Value> {
    Some(host_insight::can_signal::Value::ValF64(
        f32::from_bits(signal_value as u32) as f64 * signal_factor + signal_offset,
    ))
}

fn get_double(
    signal_value: u64,
    signal_factor: f64,
    signal_offset: f64,
) -> Option<host_insight::can_signal::Value> {
    Some(host_insight::can_signal::Value::ValF64(
        f64::from_bits(signal_value) * signal_factor + signal_offset,
    ))
}

fn get_unsigned_number(
    signal_value: u64,
    signal_factor: f64,
    signal_offset: f64,
) -> Option<host_insight::can_signal::Value> {
    if is_float(signal_factor) || is_float(signal_offset) {
        return Some(host_insight::can_signal::Value::ValF64(
            signal_value as f64 * signal_factor + signal_offset,
        ));
    }
    Some(host_insight::can_signal::Value::ValU64(
        signal_value * signal_factor as u64 + signal_offset as u64,
    ))
}

fn get_signed_number(
    signal_value: u64,
    signal_length: u64,
    signal_factor: f64,
    signal_offset: f64,
) -> Option<host_insight::can_signal::Value> {
    let signed_mask = 1 << (signal_length - 1);
    let is_negative = (signed_mask & signal_value) != 0;

    let max_val: u64 = 0xFFFFFFFFFFFFFFFF;
    let two_compliment_64 = (max_val << signal_length) | signal_value;

    if is_negative {
        if is_float(signal_factor) || is_float(signal_offset) {
            return Some(host_insight::can_signal::Value::ValF64(
                ((two_compliment_64) as i64) as f64 * signal_factor + signal_offset,
            ));
        }

        return Some(host_insight::can_signal::Value::ValI64(
            two_compliment_64 as i64 * signal_factor as i64 + signal_offset as i64,
        ));
    }

    if is_float(signal_factor) || is_float(signal_offset) {
        return Some(host_insight::can_signal::Value::ValF64(
            signal_value as f64 * signal_factor + signal_offset,
        ));
    }

    Some(host_insight::can_signal::Value::ValI64(
        signal_value as i64 * signal_factor as i64 + signal_offset as i64,
    ))
}

fn is_float(f: f64) -> bool {
    f != f as i64 as f64
}

fn get_signal_value(frame_value: u64, start_bit: u64, signal_size: u64) -> u64 {
    if signal_size == 64 {
        return frame_value;
    }

    let bit_mask: u64 = 2u64.pow(signal_size as u32) - 1;
    ((frame_value >> start_bit) & bit_mask) as u64
}
