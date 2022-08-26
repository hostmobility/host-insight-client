use async_std::task;
use elevator::elevator_client::ElevatorClient;
use elevator::{Id, Point, ResponseCode, Value, Values};
use futures::future::{try_join3, try_join_all};
use futures::stream::StreamExt;
//use futures_util::stream::StreamExt;
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
    can: CanConfig,
    gpio: GpioConfig,
    server: ServerConfig,
    time: Time,
    position: GpsData,
}

#[derive(Deserialize)]
struct ServerConfig {
    address: String,
    port: u16,
}

#[derive(Deserialize)]
struct GpioConfig {
    chip: Option<String>,
    lines: Option<Vec<u32>>,
}

#[derive(Deserialize)]
struct CanConfig {
    bitrate: u32,
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
    let bitrate = config.can.bitrate;
    eprintln!("Reading on {port}");
    eprintln!("Default bitrate: {bitrate}");

    // Add retries with backoff
    // let mut s = config.time.sleep_min_s;
    // let ms = rand::thread_rng().gen_range(0..=500);

    while let Some(frame) = socket_rx.next().await {
        eprintln!("{:#?}", frame);
    }
    Ok(ResponseCode::Exit)
}

async fn gpiomon(
    config: &Config,
    gpio_n: u32,
    //gpio_values: &HashMap<String, bool>,
    channel: Channel,
) -> Result<ResponseCode, Box<dyn Error>> {
    let mut chip = Chip::new(config.gpio.chip.clone().unwrap())?;
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse config
    let conf_str: String = fs::read_to_string("conf.toml").expect("Unable to read config file");
    let config: Config = toml::from_str(&conf_str).unwrap();

    // Connect to server
    //let server: ServerConfig = config.server;
    let lines = config.gpio.lines.as_ref().unwrap();
    let can_ports = config.can.ports.as_ref().unwrap();
    let pem = tokio::fs::read("/etc/ssl/certs/ca-certificates.crt").await?;
    let ca = Certificate::from_pem(pem);

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
    .tls_config(tls)?;

    let channel = endpoint.connect_lazy();

    // Get initial values
    let initial_vals = read_all(&config.gpio).await;

    // Add retries with backoff
    let mut s = config.time.sleep_min_s;
    let ms = rand::thread_rng().gen_range(0..=500);

    // Send initial values
    loop {
        for (i, elem) in initial_vals.iter().enumerate() {
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
    let mut gpiomon_futures = vec![gpiomon(&config, lines[0], channel.clone())];
    let mut canmon_futures = vec![canmon(&config, &can_ports[0])];
    let heartbeat_future = heartbeat(&config, channel.clone());

    for l in &lines[1..] {
        gpiomon_futures.push(gpiomon(&config, *l, channel.clone()));
    }

    for l in &can_ports[1..] {
        canmon_futures.push(canmon(&config, l));
    }

    //let futures = vec![gpiomon_futures, canmon_futures, heartbeat_future];

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
    Ok(ResponseCode::Exit)
}

async fn read_all(gpio: &GpioConfig) -> Vec<u8> {
    let line_len = gpio.lines.as_ref().unwrap().len();
    let mut chip = Chip::new(gpio.chip.clone().unwrap()).unwrap();
    let handle = chip
        .get_lines(&gpio.lines.clone().unwrap())
        .unwrap()
        .request(LineRequestFlags::INPUT, &vec![0; line_len], "multiread")
        .unwrap();
    let values = handle.get_values().unwrap();
    println!("Initial GPIO values: {:?}", values);
    values
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
