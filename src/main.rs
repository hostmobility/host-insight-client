use async_std::task;
use can_dbc::{ByteOrder, SignalExtendedValueType, MultiplexIndicator};
use elevator::elevator_client::ElevatorClient;
use elevator::{CanMessage, CanSignal, Point, ResponseCode, Value, Values, can_signal};
use futures::future::{try_join, try_join3, try_join_all};
use futures::stream::StreamExt;
use gpio_cdev::{AsyncLineEventHandle, Chip, EventRequestFlags, EventType, LineRequestFlags};
use lazy_static::lazy_static;
use rand::Rng;
use serde_derive::Deserialize;
use std::error::Error;
use std::fs;
use std::fs::File;
use std::io::prelude::*;
use std::process::Command;
use std::str;
use std::collections::HashMap;
use std::time::Duration;
use tokio_socketcan::CANSocket;
use tonic::{
    transport::{Certificate, Channel, ClientTlsConfig},
    Request, Status,
};

pub mod elevator {
    tonic::include_proto!("elevator");
}

lazy_static! {
    static ref CONFIG: Config = load_config();
}

#[derive(Deserialize)]
struct Config {
    uid: String,
    can: Option<CanConfig>,
    gpio: Option<GpioConfig>,
    server: ServerConfig,
    time: Time,
    position: GpsData,
}

#[derive(Deserialize)]
struct ServerConfig {
    address: String,
}

#[derive(Deserialize, Clone)]
struct GpioConfig {
    chip: Option<String>,
    lines: Option<Vec<u32>>,
    offset: Option<u32>,
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
    heartbeat_m: u64,
    sleep_max_s: u64,
    sleep_min_s: u64,
}

#[derive(Deserialize)]
struct GpsData {
    longitude: f64,
    latitude: f64,
}

fn intercept(mut req: Request<()>) -> Result<Request<()>, Status> {
    req.metadata_mut()
        .insert("uid", CONFIG.uid.to_string().parse().unwrap());
    Ok(req)
}

async fn send_value(
    channel: Channel,
    channel_name: &str,
    channel_vale: bool,
) -> Result<i32, Box<dyn std::error::Error>> {
    let mut client = ElevatorClient::with_interceptor(channel, intercept);

    //Create Vector "list" of Value. Value is defined in elevator.proto
    let mut v: Vec<Value> = Vec::new();

    //Create measurement of type Value
    let meas = Value {
        name: channel_name.into(),
        value: channel_vale,
    };
    //Add measurement to vector "list"
    v.push(meas);

    //Create request of type Values. Values is defined in elevator.proto
    let request = tonic::Request::new(Values { measurements: v });

    //Send values. send_values is autogenerated when elevator.proto is compiled
    //send_values is the defined RPC SendValues. Rust converts to snake_case
    let response = client.send_values(request).await?;

    Ok(response.into_inner().rc)
}

async fn send_can_message(
    channel: Channel,
    can_message: CanMessage,
) -> Result<i32, Box<dyn std::error::Error>> {
    let mut client = ElevatorClient::with_interceptor(channel, intercept);

    //Create request of type CanMessage. The latter is defined in elevator.proto
    let request = tonic::Request::new(can_message);

    let response = client.send_can_message(request).await?;
    Ok(response.into_inner().rc)
}

async fn send_point(channel: Channel) -> Result<i32, Box<dyn std::error::Error>> {
    let mut client = ElevatorClient::with_interceptor(channel, intercept);

    //Create measurement of type Value
    let point = Point {
        longitude: CONFIG.position.longitude,
        latitude: CONFIG.position.latitude,
    };

    //Send values. send_values is autogenerated when elevator.proto is compiled
    //send_values is the defined RPC SendValues. Rust converts to snail-case
    let response = client.send_position(point).await?;

    Ok(response.into_inner().rc)
}

fn load_dbc_file(s: &str) -> Result<can_dbc::DBC, Box<dyn Error>> {
    let path = home::home_dir()
        .expect("Path could not be found")
        .join(format!(".config/ada-client/{}", s));

    let mut f = File::open(path)?;
    let mut buffer = Vec::new();
    f.read_to_end(&mut buffer)?;
    let dbc = can_dbc::DBC::from_slice(&buffer).expect("Failed to parse dbc file");
    Ok(dbc)
}

// Checks if the last signal value sent is equal to supllied signal and value
fn is_can_signal_duplicate(map: &HashMap<String, Option<can_signal::Value>>, name: &String, val: &Option<can_signal::Value>) -> bool {
    if let Some(last_sent) = map.get_key_value(name) {
        if Some(last_sent.1) == Some(val) {
            return true;
        }
    }
    false
}


async fn can_monitor(port: &CanPort, channel: Channel) -> Result<ResponseCode, Box<dyn Error>> {
    let dbc = load_dbc_file(CONFIG.can.as_ref().unwrap().dbc_file.as_ref().unwrap())
        .expect("Failed to load DBC file");

    // Add retries with backoff
    let mut s = CONFIG.time.sleep_min_s;
    let ms = rand::thread_rng().gen_range(0..=500);

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
                            Some(elevator::can_signal::Value::ValStr(_)) => "enum".to_string(),
                            _ => "N/A".to_string(),
                        }
                    } else {
                        signal.unit().clone()
                    };
                    // If the signal is a multiplexor, store the value of that signal.
                    if is_multiplexor(signal) {
                        if let Some(val_enum) = can_signal_value.clone() {
                            if let can_signal::Value::ValU64(val) = val_enum {
                                multiplex_val = val;
                            }
                        }
                    }
                    // If the value is a multiplexed signal
                    // Check if the multiplex signal value matches the multiplexor value of this signal
                    // Else continue and discard the signal
                    // FIXME: This is dependent on that the multipexor signal is parsed firs in the for-loop.
                    // otherwise the multiplex_val variable will be 0
                    if is_multiplexed(signal) {
                        if let Some(val_enum) = can_signal_value.clone() {
                            if let can_signal::Value::ValU64(val) = val_enum {
                                if multiplex_val != get_multiplex_val(signal) {
                                    continue;
                                }
                            }
                        }
                    }

                    let can_signal: CanSignal = CanSignal {
                        signal_name: signal.name().clone(),
                        unit: signal_unit,
                        value: can_signal_value.clone(),
                    };
                    if is_can_signal_duplicate(&prev_map, &signal.name(), &can_signal_value) {
                        continue;
                    }
                    *prev_map.entry(signal.name().clone()).or_insert(can_signal_value.clone()) = can_signal_value.clone();
                    can_signals.push(can_signal);
                }

                if can_signals.len() == 0 {
                    continue;
                }

                let can_message: CanMessage = CanMessage {
                    bus: port.name.clone(),
                    time_stamp: None, // The tokio_socketcan library currently lacks support for timestamps, but see https://github.com/socketcan-rs/socketcan-rs/issues/22
                    signal: can_signals.clone(),
                };
                match send_can_message(channel.clone(), can_message).await {
                    Err(e) => {
                        eprintln!("Error: {e}");
                        eprintln!("Sleeping for {s}.{ms} s");
                        task::sleep(Duration::from_millis(s * 1000 + ms)).await;
                        s = std::cmp::min(s * 2, CONFIG.time.sleep_max_s);
                    }
                    Ok(r) => {
                        match ResponseCode::from_i32(r) {
                            Some(ResponseCode::CarryOn) => s = CONFIG.time.sleep_min_s,
                            Some(ResponseCode::Exit) => std::process::exit(0),
                            Some(ResponseCode::SoftwareUpdate) => {
                                println!("Software update");
                                match download(ResponseCode::SoftwareUpdate).await {
                                    Err(_) => {
                                        eprintln!("Download failed. Let's continue as if nothing happened.")
                                    }
                                    Ok(_) => std::process::exit(0),
                                }
                            }
                            _ => panic!("Unrecognized response code {r}"),
                        }
                    }
                }
            }
        }
    }
    Ok(ResponseCode::Exit)
}

async fn gpio_monitor(
    gpio_n: u32,
    //gpio_values: &HashMap<String, bool>,
    channel: Channel,
) -> Result<ResponseCode, Box<dyn Error>> {
    let mut chip = Chip::new(CONFIG.gpio.clone().unwrap().chip.unwrap())?;
    let line = chip.get_line(gpio_n)?;
    let line_offset = CONFIG.gpio.clone().unwrap().offset.unwrap_or_default();

    let mut events = AsyncLineEventHandle::new(line.events(
        LineRequestFlags::INPUT,
        EventRequestFlags::BOTH_EDGES,
        "gpioevents",
    )?)?;

    // Add retries with backoff
    let mut s = CONFIG.time.sleep_min_s;
    let ms = rand::thread_rng().gen_range(0..=500);

    while let Some(event) = events.next().await {
        match send_value(
            channel.clone(),
            &format!("Digital {}", gpio_n - line_offset),
            event?.event_type() == EventType::RisingEdge,
        )
        .await
        {
            Err(e) => {
                eprintln!("Error: {e}");
                eprintln!("Sleeping for {s}.{ms} s");
                task::sleep(Duration::from_millis(s * 1000 + ms)).await;
                s = std::cmp::min(s * 2, CONFIG.time.sleep_max_s);
            }
            Ok(r) => match ResponseCode::from_i32(r) {
                Some(ResponseCode::CarryOn) => s = CONFIG.time.sleep_min_s,
                Some(ResponseCode::Exit) => std::process::exit(0),
                Some(ResponseCode::SoftwareUpdate) => {
                    println!("Software update");
                    match download(ResponseCode::SoftwareUpdate).await {
                        Err(_) => {
                            eprintln!("Download failed. Let's continue as if nothing happened.")
                        }
                        Ok(_) => std::process::exit(0),
                    }
                }
                _ => panic!("Unrecognized response code {r}"),
            },
        }
    }
    Ok(ResponseCode::Exit)
}

async fn send_initial_values(channel: Channel, initial_gpio_vals: &Option<Vec<u8>>) {
    // Add retries with backoff
    let mut s = CONFIG.time.sleep_min_s;
    let ms = rand::thread_rng().gen_range(0..=500);

    loop {
        if initial_gpio_vals.is_some() {
            for (i, elem) in initial_gpio_vals.clone().unwrap().iter().enumerate() {
                match send_value(channel.clone(), &format!("Digital {}", i), *elem != 0).await {
                    Err(e) => {
                        eprintln!("Error: {e}");
                        eprintln!("Sleeping for {s}.{ms} s");
                        task::sleep(Duration::from_millis(s * 1000 + ms)).await;
                        s = std::cmp::min(s * 2, CONFIG.time.sleep_max_s);
                    }
                    Ok(r) => {
                        match ResponseCode::from_i32(r) {
                            Some(ResponseCode::CarryOn) => s = CONFIG.time.sleep_min_s,
                            Some(ResponseCode::Exit) => std::process::exit(0),
                            Some(ResponseCode::SoftwareUpdate) => {
                                println!("Software update");
                                match download(ResponseCode::SoftwareUpdate).await {
                                    Err(_) => {
                                        eprintln!("Download failed. Let's continue as if nothing happened.")
                                    }
                                    Ok(_) => std::process::exit(0),
                                }
                            }
                            Some(ResponseCode::ConfigUpdate) => {
                                println!("Config update");
                                match download(ResponseCode::ConfigUpdate).await {
                                    Err(_) => {
                                        eprintln!("Download failed. Let's continue as if nothing happened.")
                                    }
                                    Ok(_) => std::process::exit(0),
                                }
                            }
                            _ => panic!("Unrecognized response code {r}"),
                        }
                    }
                }
            }
        }
        // Send GPS position
        match send_point(channel.clone()).await {
            Err(e) => {
                eprintln!("Error: {e}");
                eprintln!("Sleeping for {s}.{ms} s");
                task::sleep(Duration::from_millis(s * 1000 + ms)).await;
                s = std::cmp::min(s * 2, CONFIG.time.sleep_max_s);
            }
            Ok(r) => match ResponseCode::from_i32(r) {
                Some(ResponseCode::CarryOn) => break,
                Some(ResponseCode::Exit) => std::process::exit(0),
                Some(ResponseCode::SoftwareUpdate) => {
                    println!("Software update");
                    match download(ResponseCode::ConfigUpdate).await {
                        Err(_) => {
                            eprintln!("Download failed. Let's continue as if nothing happened.")
                        }
                        Ok(_) => std::process::exit(0),
                    }
                }
                Some(ResponseCode::ConfigUpdate) => {
                    println!("Config update");
                    match download(ResponseCode::ConfigUpdate).await {
                        Err(_) => {
                            eprintln!("Download failed. Let's continue as if nothing happened.")
                        }
                        Ok(_) => std::process::exit(0),
                    }
                }

                _ => panic!("Unrecognized response code {r}"),
            },
        }
    }
}

fn setup_can() {
    for p in CONFIG.can.clone().unwrap().ports.unwrap() {
        let interface = p.name;

        let mut bitrate = "500000".to_string();
        if p.bitrate.is_some() {
            bitrate = p.bitrate.unwrap().to_string();
        }

        // ip link set INTERFACE down
        let mut process = Command::new("ip")
            .arg("link")
            .arg("set")
            .arg(&interface)
            .arg("down")
            .spawn()
            .ok()
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
        let mut process = Command::new("ip")
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
            .ok()
            .expect("Failed to run ip command.");
        match process.wait() {
            Ok(_) => eprintln!("Interface {} is up", &interface),
            Err(e) => panic!("Error: {}", e),
        }
    }
}

async fn setup_server() -> Channel {
    // Connect to server
    //let server: ServerConfig = CONFIG.server;
    let pem = tokio::fs::read("/etc/ssl/certs/ca-certificates.crt").await;
    let ca = Certificate::from_pem(pem.unwrap());

    let tls = ClientTlsConfig::new()
        .ca_certificate(ca)
        .domain_name(CONFIG.server.address.clone());

    let endpoint = Channel::builder(
        format!("https://{}", CONFIG.server.address.clone())
            .parse()
            .unwrap(),
    )
    .tls_config(tls)
    .unwrap();

    endpoint.connect_lazy()
}

fn load_config() -> Config {
    let local_conf = home::home_dir()
        .expect("Could not find home directory")
        .join(".config/ada-client/conf.toml");
    let fallback_conf = "/etc/opt/ada-client/conf.toml";

    toml::from_str(
        &fs::read_to_string(local_conf)
            .unwrap_or_else(|_| fs::read_to_string(fallback_conf).unwrap()),
    )
    .expect("Failed to load any config file.")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let channel = setup_server().await;

    // Get and send initial GPIO values
    let initial_gpio_vals: Option<Vec<u8>> = read_all().await;
    send_initial_values(channel.clone(), &initial_gpio_vals).await;

    let heartbeat_future = heartbeat(channel.clone());

    // TODO: refactor this ugly part
    if initial_gpio_vals.is_some() && CONFIG.can.is_some() {
        let lines = CONFIG.gpio.clone().unwrap().lines.unwrap();
        let mut gpio_monitor_futures = vec![gpio_monitor(lines[0], channel.clone())];
        for l in &lines[1..] {
            gpio_monitor_futures.push(gpio_monitor(*l, channel.clone()));
        }

        setup_can();
        let can_ports = CONFIG.can.clone().unwrap().ports.unwrap();
        let mut can_monitor_futures = vec![can_monitor(&can_ports[0], channel.clone())];
        for p in &can_ports[1..] {
            can_monitor_futures.push(can_monitor(p, channel.clone()));
        }
        match try_join3(
            try_join_all(gpio_monitor_futures),
            try_join_all(can_monitor_futures),
            heartbeat_future,
        )
        .await
        {
            Ok(_) => eprintln!("All tasks completed successfully"),
            Err(e) => eprintln!("Some task failed: {e}"),
        };
    } else if CONFIG.can.is_some() {
        setup_can();
        let can_ports = CONFIG.can.clone().unwrap().ports.unwrap();
        let mut can_monitor_futures = vec![can_monitor(&can_ports[0], channel.clone())];
        for p in &can_ports[1..] {
            can_monitor_futures.push(can_monitor(p, channel.clone()));
        }
        match try_join(try_join_all(can_monitor_futures), heartbeat_future).await {
            Ok(_) => eprintln!("All tasks completed successfully"),
            Err(e) => eprintln!("Some task failed: {e}"),
        };
    } else if initial_gpio_vals.is_some() {
        let lines = CONFIG.gpio.clone().unwrap().lines.unwrap();
        let mut gpio_monitor_futures = vec![gpio_monitor(lines[0], channel.clone())];
        for l in &lines[1..] {
            gpio_monitor_futures.push(gpio_monitor(*l, channel.clone()));
        }

        match try_join(try_join_all(gpio_monitor_futures), heartbeat_future).await {
            Ok(_) => eprintln!("All tasks completed successfully"),
            Err(e) => eprintln!("Some task failed: {e}"),
        };
    } else {
        eprintln!("Invalid configuration. You need to specify at least one of the following I/Os: gpio, can");
    }

    Ok(())
}

async fn heartbeat(channel: Channel) -> Result<ResponseCode, Box<dyn Error>> {
    let mut client = ElevatorClient::with_interceptor(channel, intercept);

    loop {
        let status = elevator::Status { code: 0 }; // Always report OK for now.
        task::sleep(Duration::from_secs(CONFIG.time.heartbeat_m * 60)).await;

        match client.heart_beat(status).await {
            Ok(r) => match ResponseCode::from_i32(r.into_inner().rc) {
                Some(ResponseCode::CarryOn) => continue,
                Some(ResponseCode::Exit) => std::process::exit(0),
                Some(ResponseCode::SoftwareUpdate) => {
                    println!("Software update");
                    match download(ResponseCode::SoftwareUpdate).await {
                        Err(_) => {
                            eprintln!("Download failed. Let's continue as if nothing happened.")
                        }
                        Ok(_) => std::process::exit(0),
                    }
                }
                Some(ResponseCode::ConfigUpdate) => {
                    println!("Config update");
                    match download(ResponseCode::ConfigUpdate).await {
                        Err(_) => {
                            eprintln!("Download failed. Let's continue as if nothing happened.")
                        }
                        Ok(_) => std::process::exit(0),
                    }
                }

                _ => panic!("Unrecognized response code"),
            },
            Err(e) => eprintln!("The server could not receive the heart beat. Status: {e}"),
        };
    }
}

async fn read_all() -> Option<Vec<u8>> {
    let chip = CONFIG.gpio.as_ref()?.clone().chip;
    let lines = CONFIG.gpio.as_ref()?.clone().lines;

    match (chip, lines) {
        (Some(chip), Some(lines)) => {
            let chip = Chip::new(chip);
            if chip.is_err() {
                eprintln!("Error {:?}", chip.err());
                None
            } else {
                let l = chip.unwrap().get_lines(&lines);
                if l.is_err() {
                    eprintln!("Error {:?}", l.err());
                    None
                } else {
                    let handle = l
                        .unwrap()
                        .request(LineRequestFlags::INPUT, &vec![0; lines.len()], "multiread")
                        .unwrap();
                    let values = handle.get_values().unwrap();
                    eprintln!("Initial GPIO values: {:?}", values);
                    Some(values)
                }
            }
        }
        _ => None,
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
) -> Option<elevator::can_signal::Value> {
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
        Some(SignalValueType::FLOAT) => get_float(signal_value, *s.factor(), *s.offset()),
        Some(SignalValueType::SIGNED) => {
            get_signed_number(signal_value, *s.signal_size(), *s.factor(), *s.offset())
        }
        Some(SignalValueType::UNSIGNED) => {
            get_unsigned_number(signal_value, *s.factor(), *s.offset())
        }
        Some(SignalValueType::DOUBLE) => get_double(signal_value, *s.factor(), *s.offset()),
        // FIXME: IMPLEMENT BOOL
        Some(SignalValueType::STRING) => get_string(signal_value, dbc, id, s),
        _ => None,
    }
}


fn is_multiplexor(s: &can_dbc::Signal) -> bool {
    match s.multiplexer_indicator() {
        can_dbc::MultiplexIndicator::Multiplexor => {
            return true;
        },
        can_dbc::MultiplexIndicator::MultiplexedSignal(val) => {
            return false;
        },
        can_dbc::MultiplexIndicator::MultiplexorAndMultiplexedSignal(val) => {
            return false;
        },
        can_dbc::MultiplexIndicator::Plain => {
            return false;
        },
    }
}

fn is_multiplexed(s: &can_dbc::Signal) -> bool {
    match s.multiplexer_indicator() {
        can_dbc::MultiplexIndicator::Multiplexor => {
            return false;
        },
        can_dbc::MultiplexIndicator::MultiplexedSignal(val) => {
            return true;
        },
        can_dbc::MultiplexIndicator::MultiplexorAndMultiplexedSignal(val) => {
            return false;
        },
        can_dbc::MultiplexIndicator::Plain => {
            return false;
        },
    }
}

fn get_multiplex_val(s: &can_dbc::Signal) -> u64 {
    match s.multiplexer_indicator() {
        can_dbc::MultiplexIndicator::Multiplexor => {
            return 0;
        },
        can_dbc::MultiplexIndicator::MultiplexedSignal(val) => {
            return *val;
        },
        can_dbc::MultiplexIndicator::MultiplexorAndMultiplexedSignal(val) => {
            return *val;
        },
        can_dbc::MultiplexIndicator::Plain => {
            return 0;
        },
    }
}


#[derive(Debug)]
enum SignalValueType {
    FLOAT,
    SIGNED,
    UNSIGNED,
    DOUBLE,
    // BOOL,  UNIMPLEMENTED
    STRING,
}

fn get_signal_value_type(
    s: &can_dbc::Signal,
    dbc: &can_dbc::DBC,
    id: &can_dbc::MessageId,
) -> Option<SignalValueType> {
    let val_desc = dbc.value_descriptions_for_signal(*id, s.name());
    if val_desc.is_some() {
        return Some(SignalValueType::STRING);
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
        Some(SignalExtendedValueType::IEEEfloat32Bit) => Some(SignalValueType::FLOAT),
        Some(SignalExtendedValueType::IEEEdouble64bit) => Some(SignalValueType::DOUBLE),
        Some(SignalExtendedValueType::SignedOrUnsignedInteger) => match *s.value_type() {
            can_dbc::ValueType::Unsigned => Some(SignalValueType::UNSIGNED),
            can_dbc::ValueType::Signed => Some(SignalValueType::SIGNED),
        },
        _ => None,
    }
}

fn get_string(
    signal_value: u64,
    dbc: &can_dbc::DBC,
    id: &can_dbc::MessageId,
    s: &can_dbc::Signal,
) -> Option<elevator::can_signal::Value> {
    let val_desc = dbc.value_descriptions_for_signal(*id, s.name());

    if let Some(desc) = val_desc {
        for elem in desc {
            if *elem.a() == signal_value as f64 {
                return Some(elevator::can_signal::Value::ValStr(elem.b().to_string()));
            }
        }
        // Signal exists in value description but key could not be found
        return Some(elevator::can_signal::Value::ValStr(
            signal_value.to_string(),
        ));
    }
    None
}

fn get_float(
    signal_value: u64,
    signal_factor: f64,
    signal_offset: f64,
) -> Option<elevator::can_signal::Value> {
    Some(elevator::can_signal::Value::ValF64(
        f32::from_bits(signal_value as u32) as f64 * signal_factor + signal_offset,
    ))
}

fn get_double(
    signal_value: u64,
    signal_factor: f64,
    signal_offset: f64,
) -> Option<elevator::can_signal::Value> {
    Some(elevator::can_signal::Value::ValF64(
        f64::from_bits(signal_value) * signal_factor + signal_offset,
    ))
}

fn get_unsigned_number(
    signal_value: u64,
    signal_factor: f64,
    signal_offset: f64,
) -> Option<elevator::can_signal::Value> {
    if is_float(signal_factor) || is_float(signal_offset) {
        return Some(elevator::can_signal::Value::ValF64(
            signal_value as f64 * signal_factor + signal_offset,
        ));
    }
    Some(elevator::can_signal::Value::ValU64(
        signal_value * signal_factor as u64 + signal_offset as u64,
    ))
}

fn get_signed_number(
    signal_value: u64,
    signal_length: u64,
    signal_factor: f64,
    signal_offset: f64,
) -> Option<elevator::can_signal::Value> {
    let signed_mask = 1 << (signal_length - 1);
    let is_negative = (signed_mask & signal_value) != 0;

    let max_val: u64 = 0xFFFFFFFFFFFFFFFF;
    let two_compliment_64 = (max_val << signal_length) | signal_value;

    if is_negative {
        if is_float(signal_factor) || is_float(signal_offset) {
            return Some(elevator::can_signal::Value::ValF64(
                ((two_compliment_64) as i64) as f64 * signal_factor + signal_offset,
            ));
        }

        return Some(elevator::can_signal::Value::ValI64(
            two_compliment_64 as i64 * signal_factor as i64
                + signal_offset as i64,
        ));
    }

    if is_float(signal_factor) || is_float(signal_offset) {
        return Some(elevator::can_signal::Value::ValF64(
            signal_value as f64 * signal_factor + signal_offset,
        ));
    }

    Some(elevator::can_signal::Value::ValI64(
        signal_value as i64 * signal_factor as i64 + signal_offset as i64,
    ))
}

async fn download(code: ResponseCode) -> Result<(), std::io::Error> {
    if code == ResponseCode::SoftwareUpdate {
        let mut process = Command::new("curl")
            .arg("-o")
            .arg("ada-client-new")
            .arg("https://hm.fps-gbg.net/files/ada/ada-client")
            .spawn()
            .ok()
            .expect("Failed to execute curl.");
        match process.wait() {
            Ok(_) => println!("Download completed"),
            Err(e) => return Err(e),
        }
    } else if code == ResponseCode::ConfigUpdate {
        let local_conf_dir = home::home_dir()
            .expect("Could not find home directory")
            .join(".config/ada-client/");
        fs::create_dir_all(&local_conf_dir)?;
        let new_local_conf = local_conf_dir.join("conf.toml-new");
        let mut process = Command::new("curl")
            .arg("-o")
            .arg(new_local_conf)
            .arg("https://hm.fps-gbg.net/files/ada/conf.toml")
            .spawn()
            .ok()
            .expect("Failed to execute curl.");
        match process.wait() {
            Ok(_) => println!("Download completed"),
            Err(e) => return Err(e),
        }
    }

    Ok(())
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
