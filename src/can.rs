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
use async_std::sync::Mutex;
use can_dbc::{ByteOrder, MultiplexIndicator, SignalExtendedValueType};
use futures::{stream, stream::StreamExt};
use lazy_static::lazy_static;
use lib::{
    host_insight::{agent_client::AgentClient, can_signal, CanMessage, CanSignal},
    CanPort, ExitCodes, CONFIG, CONF_DIR,
};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::sleep;
use tokio_socketcan::CANSocket;
use tonic::transport::Channel;
use tonic::Request;

lazy_static! {
    static ref CAN_MSG_QUEUE: Mutex<Vec<CanMessage>> = Mutex::new(Vec::new());
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

pub async fn can_sender(channel: Channel) -> Result<(), Box<dyn Error>> {
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

pub async fn can_monitor(port: &CanPort) -> Result<(), Box<dyn Error>> {
    let dbc = load_dbc_file(CONFIG.can.as_ref().unwrap().dbc_file.as_ref().unwrap())
        .unwrap_or_else(|_| std::process::exit(ExitCodes::Enoent as i32));

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
                            Some(can_signal::Value::ValStr(_)) => "enum".to_string(),
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

pub fn setup_can(ports: &Vec<CanPort>) {
    let default_bitrate = "500000";
    let default_listen_only_state = "on";

    for p in ports {
        let interface = &p.name;

        let bitrate = if let Some(b) = p.bitrate {
            b.to_string()
        } else {
            default_bitrate.to_string()
        };

        // ip link set INTERFACE down
        let mut process = std::process::Command::new("ip")
            .arg("link")
            .arg("set")
            .arg(interface)
            .arg("down")
            .spawn()
            .expect("Failed to run ip command.");
        match process.wait() {
            Ok(_) => eprintln!("Interface {} is down", &interface),
            Err(e) => panic!("Error: {}", e),
        }

        // ip link set up INTERFACE type can bitrate BITRATE listen-only {ON/OFF}
        let listen_only_state = match p.listen_only {
            Some(true) => "on",
            Some(false) => "off",
            None => default_listen_only_state,
        };

        let mut process = std::process::Command::new("ip")
            .arg("link")
            .arg("set")
            .arg("up")
            .arg(interface)
            .arg("type")
            .arg("can")
            .arg("bitrate")
            .arg(bitrate)
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

// Get the can signal value based on the message ID, the data part of
// the frame, the signal, and extra metadata contained in the DBC
// file.
// The following can_signal::can_signal::Value types can be returned:
//   can_signal::Value::ValF64, ValStr, ValI64, ValU64
fn get_can_signal_value(
    id: &can_dbc::MessageId,
    d: &[u8],
    s: &can_dbc::Signal,
    dbc: &can_dbc::DBC,
) -> Option<can_signal::Value> {
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
) -> Option<can_signal::Value> {
    let val_desc = dbc.value_descriptions_for_signal(*id, s.name());

    if let Some(desc) = val_desc {
        for elem in desc {
            if *elem.a() == signal_value as f64 {
                return Some(can_signal::Value::ValStr(elem.b().to_string()));
            }
        }
        // Signal exists in value description but key could not be found
        return Some(can_signal::Value::ValStr(signal_value.to_string()));
    }
    None
}

fn get_float(
    signal_value: u64,
    signal_factor: f64,
    signal_offset: f64,
) -> Option<can_signal::Value> {
    Some(can_signal::Value::ValF64(
        f32::from_bits(signal_value as u32) as f64 * signal_factor + signal_offset,
    ))
}

fn get_double(
    signal_value: u64,
    signal_factor: f64,
    signal_offset: f64,
) -> Option<can_signal::Value> {
    Some(can_signal::Value::ValF64(
        f64::from_bits(signal_value) * signal_factor + signal_offset,
    ))
}

fn get_unsigned_number(
    signal_value: u64,
    signal_factor: f64,
    signal_offset: f64,
) -> Option<can_signal::Value> {
    if is_float(signal_factor) || is_float(signal_offset) {
        return Some(can_signal::Value::ValF64(
            signal_value as f64 * signal_factor + signal_offset,
        ));
    }
    Some(can_signal::Value::ValU64(
        signal_value * signal_factor as u64 + signal_offset as u64,
    ))
}

fn get_signed_number(
    signal_value: u64,
    signal_length: u64,
    signal_factor: f64,
    signal_offset: f64,
) -> Option<can_signal::Value> {
    let signed_mask = 1 << (signal_length - 1);
    let is_negative = (signed_mask & signal_value) != 0;

    let max_val: u64 = 0xFFFFFFFFFFFFFFFF;
    let two_compliment_64 = (max_val << signal_length) | signal_value;

    if is_negative {
        if is_float(signal_factor) || is_float(signal_offset) {
            return Some(can_signal::Value::ValF64(
                ((two_compliment_64) as i64) as f64 * signal_factor + signal_offset,
            ));
        }

        return Some(can_signal::Value::ValI64(
            two_compliment_64 as i64 * signal_factor as i64 + signal_offset as i64,
        ));
    }

    if is_float(signal_factor) || is_float(signal_offset) {
        return Some(can_signal::Value::ValF64(
            signal_value as f64 * signal_factor + signal_offset,
        ));
    }

    Some(can_signal::Value::ValI64(
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
    (frame_value >> start_bit) & bit_mask
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
