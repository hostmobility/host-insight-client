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
use futures::future::FutureExt;
use gpio::{digital_in_monitor, remote_control_monitor, set_all_digital_out_to_defaults};
use lib::{CONFIG, GIT_COMMIT_DESCRIBE};
use net::{heartbeat, send_initial_values, setup_network};
use std::error::Error;
use utils::clean_up;

mod can;
mod gpio;
mod net;
mod utils;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    command!().version(GIT_COMMIT_DESCRIBE).get_matches();

    println!("Starting HOST Insight Client {}", GIT_COMMIT_DESCRIBE);
    let channel = setup_network().await;

    if CONFIG.digital_out.is_some() {
        set_all_digital_out_to_defaults()?;
    }

    // Send state and any initial Digital IN values
    send_initial_values(channel.clone()).await;

    let mut all_futures: Vec<Box<dyn FnOnce() -> Vec<_>>> = vec![];

    if let Some(can_config) = &CONFIG.can {
        if let Some(ports) = &can_config.ports {
            setup_can(ports);

            let can_monitor_futures: Vec<_> = ports
                .iter()
                .map(can_monitor)
                .map(|future| future.boxed())
                .collect();
            all_futures.push(Box::new(|| can_monitor_futures));

            let can_sender_futures: Vec<_> = vec![can_sender(channel.clone()).boxed()];
            all_futures.push(Box::new(|| can_sender_futures));
        }
    }

    if let Some(digital_in_config) = &CONFIG.digital_in {
        if let Some(ports) = &digital_in_config.ports {
            let digital_in_monitor_futures: Vec<_> = ports
                .iter()
                .map(|port| digital_in_monitor(port, channel.clone()))
                .map(|future| future.boxed())
                .collect();
            all_futures.push(Box::new(|| digital_in_monitor_futures));
        }
        let remote_control_futures: Vec<_> = vec![remote_control_monitor(channel.clone()).boxed()];
        all_futures.push(Box::new(|| remote_control_futures));
    }

    // Always add heartbeat
    let remote_control_futures: Vec<_> = vec![heartbeat(channel.clone()).boxed()];
    all_futures.push(Box::new(|| remote_control_futures));

    let flattened_futures: Vec<_> = all_futures.into_iter().flat_map(|f| f()).collect();

    match try_join_all(flattened_futures).await {
        Ok(_) => eprintln!("All tasks completed successfully"),
        Err(e) => eprintln!("Some task failed: {e}"),
    };

    clean_up();
    Ok(())
}
