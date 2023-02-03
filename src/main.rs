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

use can::{can_monitor, can_sender, setup_can};
use clap::command;
use futures::future::try_join_all;
use gpio::{digital_in_monitor, remote_control_monitor, set_all_digital_out_to_defaults};
use lib::{CONFIG, GIT_COMMIT_DESCRIBE};
use net::{heartbeat, send_initial_values, setup_network};
use utils::clean_up;

mod can;
mod gpio;
mod net;
mod utils;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    command!().version(GIT_COMMIT_DESCRIBE).get_matches();

    println!("Starting HOST Insight Client {}", GIT_COMMIT_DESCRIBE);
    let channel = setup_network().await;

    if CONFIG.can.is_some() {
        setup_can();
    }

    if CONFIG.digital_out.is_some() {
        set_all_digital_out_to_defaults()?;
    }

    // Send state and any initial Digital IN values
    send_initial_values(channel.clone()).await;

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
