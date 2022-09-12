use async_std::task;
use elevator::elevator_client::ElevatorClient;
use elevator::{Id, Point, ResponseCode, Value, Values};
use futures::future::{try_join, try_join3, try_join_all};
use futures::stream::StreamExt;
use gpio_cdev::{AsyncLineEventHandle, Chip, EventRequestFlags, EventType, LineRequestFlags};
use rand::Rng;
use serde_derive::Deserialize;
use std::error::Error;
use std::fs;
use std::process::Command;
use std::time::Duration;
use tokio_socketcan::CANSocket;
use tonic::transport::{Certificate, Channel, ClientTlsConfig};

pub mod elevator {
    tonic::include_proto!("elevator");
}

#[derive(Deserialize)]
struct Config {
    uid: u32,
    can: Option<CanConfig>,
    gpio: Option<GpioConfig>,
    server: ServerConfig,
    time: Time,
    position: GpsData,
}

#[derive(Deserialize)]
struct ServerConfig {
    address: String,
    port: u16,
}

#[derive(Deserialize, Clone)]
struct GpioConfig {
    chip: Option<String>,
    lines: Option<Vec<u32>>,
}

#[derive(Deserialize, Clone)]
struct CanConfig {
    bitrate: Option<u32>,
    ports: Option<Vec<String>>,
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

const GPIO_LINE_OFFSET: u32 = 5;

async fn send_value(
    uid: u32,
    channel: Channel,
    channel_name: &str,
    channel_vale: bool,
) -> Result<i32, Box<dyn std::error::Error>> {
    let mut client = ElevatorClient::new(channel);

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
    let request = tonic::Request::new(Values {
        unit_id: uid.to_string(),
        measurements: v,
    });
    //Send values. send_values is autogenerated when elevator.proto is compiled
    //send_values is the defined RPC SendValues. Rust converts to snake_case
    let response = client.send_values(request).await?;

    println!("RESPONSE={:?}", response);
    Ok(response.into_inner().rc)
}

async fn send_point(
    uid: u32,
    channel: Channel,
    config: &Config,
) -> Result<i32, Box<dyn std::error::Error>> {
    let mut client = ElevatorClient::new(channel);

    //Create measurement of type Value
    let point = Point {
        unit_id: uid.to_string(),
        longitude: config.position.longitude,
        latitude: config.position.latitude,
    };

    //Send values. send_values is autogenerated when elevator.proto is compiled
    //send_values is the defined RPC SendValues. Rust converts to snail-case
    let response = client.send_position(point).await?;

    println!("RESPONSE={:?}", response);
    Ok(response.into_inner().rc)
}

async fn canmon(config: &Config, port: &str) -> Result<ResponseCode, Box<dyn Error>> {
    let mut socket_rx = CANSocket::open(port)?;
    eprintln!("Start reading on {port}");
    if let Some(bitrate) = config.can.as_ref().unwrap().bitrate {
        eprintln!("Default bitrate: {bitrate}");
    }
    while let Some(frame) = socket_rx.next().await {
        eprintln!("{:#?}", frame);
    }

    // Add retries with backoff
    // let mut s = config.time.sleep_min_s;
    // let ms = rand::thread_rng().gen_range(0..=500);

    Ok(ResponseCode::Exit)
}

async fn gpiomon(
    config: &Config,
    gpio_n: u32,
    //gpio_values: &HashMap<String, bool>,
    channel: Channel,
) -> Result<ResponseCode, Box<dyn Error>> {
    let mut chip = Chip::new(config.gpio.clone().unwrap().chip.unwrap())?;
    let line = chip.get_line(gpio_n)?;
    let mut events = AsyncLineEventHandle::new(line.events(
        LineRequestFlags::INPUT,
        EventRequestFlags::BOTH_EDGES,
        "gpioevents",
    )?)?;

    // Add retries with backoff
    let mut s = config.time.sleep_min_s;
    let ms = rand::thread_rng().gen_range(0..=500);

    while let Some(event) = events.next().await {
        match send_value(
            config.uid,
            channel.clone(),
            &format!("Digital {}", gpio_n - GPIO_LINE_OFFSET),
            event?.event_type() == EventType::RisingEdge,
        )
        .await
        {
            Err(e) => {
                eprintln!("Error: {e}");
                eprintln!("Sleeping for {s}.{ms} s");
                task::sleep(Duration::from_millis(s * 1000 + ms)).await;
                s = std::cmp::min(s * 2, config.time.sleep_max_s);
            }
            Ok(r) => match ResponseCode::from_i32(r) {
                Some(ResponseCode::CarryOn) => s = config.time.sleep_min_s,
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

async fn send_initial_values(
    config: &Config,
    channel: Channel,
    initial_gpio_vals: &Option<Vec<u8>>,
) {
    // Add retries with backoff
    let mut s = config.time.sleep_min_s;
    let ms = rand::thread_rng().gen_range(0..=500);

    loop {
        if initial_gpio_vals.is_some() {
            for (i, elem) in initial_gpio_vals.clone().unwrap().iter().enumerate() {
                match send_value(
                    config.uid,
                    channel.clone(),
                    &format!("Digital {}", i),
                    *elem != 0,
                )
                .await
                {
                    Err(e) => {
                        eprintln!("Error: {e}");
                        eprintln!("Sleeping for {s}.{ms} s");
                        task::sleep(Duration::from_millis(s * 1000 + ms)).await;
                        s = std::cmp::min(s * 2, config.time.sleep_max_s);
                    }
                    Ok(r) => {
                        match ResponseCode::from_i32(r) {
                            Some(ResponseCode::CarryOn) => s = config.time.sleep_min_s,
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
        match send_point(config.uid, channel.clone(), &config).await {
            Err(e) => {
                eprintln!("Error: {e}");
                eprintln!("Sleeping for {s}.{ms} s");
                task::sleep(Duration::from_millis(s * 1000 + ms)).await;
                s = std::cmp::min(s * 2, config.time.sleep_max_s);
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

async fn setup_server(config: &Config) -> Channel {
    // Connect to server
    //let server: ServerConfig = config.server;
    let pem = tokio::fs::read("/etc/ssl/certs/ca-certificates.crt").await;
    let ca = Certificate::from_pem(pem.unwrap());

    let tls = ClientTlsConfig::new()
        .ca_certificate(ca)
        .domain_name(config.server.address.clone());

    let endpoint = Channel::builder(
        format!(
            "https://{}:{}",
            &config.server.address.clone(),
            config.server.port
        )
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
    let config = load_config();

    let channel = setup_server(&config).await;

    // Get and send initial GPIO values
    let initial_gpio_vals: Option<Vec<u8>> = read_all(&config).await;
    send_initial_values(&config, channel.clone(), &initial_gpio_vals).await;

    let heartbeat_future = heartbeat(&config, channel.clone());

    // TODO: refactor this ugly part
    if initial_gpio_vals.is_some() && config.can.is_some() {
        let lines = config.gpio.clone().unwrap().lines.unwrap();
        let mut gpiomon_futures = vec![gpiomon(&config, lines[0], channel.clone())];
        for l in &lines[1..] {
            gpiomon_futures.push(gpiomon(&config, *l, channel.clone()));
        }

        let can_ports = config.can.clone().unwrap().ports.unwrap();
        let mut canmon_futures = vec![canmon(&config, &can_ports[0])];
        for l in &can_ports[1..] {
            canmon_futures.push(canmon(&config, l));
        }
        match try_join3(
            try_join_all(gpiomon_futures),
            try_join_all(canmon_futures),
            heartbeat_future,
        )
        .await
        {
            Ok(_) => eprintln!("All tasks completed successfully"),
            Err(e) => eprintln!("Some task failed: {e}"),
        };
    } else if config.can.is_some() {
        let can_ports = config.can.clone().unwrap().ports.unwrap();
        let mut canmon_futures = vec![canmon(&config, &can_ports[0])];
        for l in &can_ports[1..] {
            canmon_futures.push(canmon(&config, l));
        }
        match try_join(try_join_all(canmon_futures), heartbeat_future).await {
            Ok(_) => eprintln!("All tasks completed successfully"),
            Err(e) => eprintln!("Some task failed: {e}"),
        };
    } else if initial_gpio_vals.is_some() {
        let lines = config.gpio.clone().unwrap().lines.unwrap();
        let mut gpiomon_futures = vec![gpiomon(&config, lines[0], channel.clone())];
        for l in &lines[1..] {
            gpiomon_futures.push(gpiomon(&config, *l, channel.clone()));
        }

        match try_join(try_join_all(gpiomon_futures), heartbeat_future).await {
            Ok(_) => eprintln!("All tasks completed successfully"),
            Err(e) => eprintln!("Some task failed: {e}"),
        };
    } else {
        eprintln!("Invalid configuration. You need to specify at least one of the following I/Os: gpio, can");
    }

    Ok(())
}

async fn heartbeat(config: &Config, channel: Channel) -> Result<ResponseCode, Box<dyn Error>> {
    let mut client = ElevatorClient::new(channel);

    //Create measurement of type Value
    let id = Id {
        unit_id: config.uid.to_string(),
    };

    loop {
        task::sleep(Duration::from_secs(config.time.heartbeat_m * 60)).await;

        match client.heart_beat(id.clone()).await {
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

async fn read_all(config: &Config) -> Option<Vec<u8>> {
    let chip = config.gpio.clone().unwrap().chip;
    let lines = config.gpio.clone().unwrap().lines;

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

async fn download(code: ResponseCode) -> Result<(), std::io::Error> {
    if code == ResponseCode::SoftwareUpdate {
        let mut process = Command::new("curl")
            .arg("-o")
            .arg("client-new")
            .arg("https://hm.fps-gbg.net/files/ada/client")
            .spawn()
            .ok()
            .expect("Failed to execute curl.");
        match process.wait() {
            Ok(_) => println!("Download completed"),
            Err(e) => return Err(e),
        }
    } else if code == ResponseCode::ConfigUpdate {
        let mut process = Command::new("curl")
            .arg("-o")
            .arg("conf.toml-new")
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
