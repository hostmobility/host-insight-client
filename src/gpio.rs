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

use super::net::{handle_send_result, intercept};
use async_lock::Barrier;
use async_std::sync::Mutex;
use futures::stream::StreamExt;
use gpio_cdev::{AsyncLineEventHandle, Chip, EventRequestFlags, EventType, LineRequestFlags};
use lazy_static::lazy_static;
use lib::{
    host_insight::{
        agent_client::AgentClient, remote_control_client::RemoteControlClient, ControlStatus,
        GpioState, UnitControlStatus, Value, Values,
    },
    DigitalInPort, DigitalOutPort, CONFIG,
};
use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;
use tonic::transport::Channel;
use tonic::Request;

lazy_static! {
    static ref DIGITAL_OUT_MAP: Option<HashMap<String, DigitalOutPort>> = create_digital_out_map();
    pub static ref REMOTE_CONTROL_BARRIER: Arc<Barrier> = Arc::new(Barrier::new(2));
    pub static ref REMOTE_CONTROL_IN_PROCESS: Mutex<bool> = Mutex::new(false);
}

// Get some HashMap of <external name, value> or None
pub async fn read_all_digital_in() -> Option<HashMap<String, u8>> {
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

pub async fn remote_control_monitor(channel: Channel) -> Result<(), Box<dyn Error>> {
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

pub async fn digital_in_monitor(
    port: &DigitalInPort,
    channel: Channel,
) -> Result<(), Box<dyn Error>> {
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

pub fn set_all_digital_out_to_defaults() -> Result<(), gpio_cdev::Error> {
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

pub async fn send_value(channel: Channel, channel_name: &str, channel_vale: u8) {
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
