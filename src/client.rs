use async_std::task;
use elevator::elevator_client::ElevatorClient;
use elevator::{Id, Point, ResponseCode, Value, Values};
use futures::future::{try_join, try_join_all};
use futures::stream::StreamExt;
use gpio_cdev::{AsyncLineEventHandle, Chip, EventRequestFlags, EventType, LineRequestFlags};
use rand::Rng;
use serde_derive::Deserialize;
use std::error::Error;
use std::fs;
use std::process::Command;
use std::time::Duration;
use tonic::transport::{Certificate, Channel, ClientTlsConfig};

pub mod elevator {
    tonic::include_proto!("elevator");
}

#[derive(Deserialize)]
struct Config {
    uid: u32,
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
                if s <= config.time.sleep_max_s {
                    eprintln!("Sleeping for {s}.{ms} s");
                    task::sleep(Duration::from_millis(s * 1000 + ms)).await;
                    s = std::cmp::min(s * 2, config.time.sleep_max_s);
                } else {
                    return Err(e);
                }
            }
            Ok(r) => match ResponseCode::from_i32(r) {
                Some(ResponseCode::CarryOn) => s = config.time.sleep_min_s,
                Some(ResponseCode::Exit) => break,
                Some(ResponseCode::SoftwareUpdate) => {
                    println!("Software update");
                    match download().await {
                        Err(_) => {
                            eprintln!("Download failed. Let's continue as if nothing happened.")
                        }
                        Ok(_) => break,
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
                    if s <= config.time.sleep_max_s {
                        eprintln!("Error: {e}");
                        eprintln!("Sleeping for {s}.{ms} s");
                        task::sleep(Duration::from_millis(s * 1000 + ms)).await;
                        s = std::cmp::min(s * 2, config.time.sleep_max_s);
                    } else {
                        eprintln!("Error: {e}");
                        return Err(e);
                    }
                }
                Ok(r) => match ResponseCode::from_i32(r) {
                    Some(ResponseCode::CarryOn) => s = config.time.sleep_min_s,
                    Some(ResponseCode::Exit) => return Ok(()),
                    Some(ResponseCode::SoftwareUpdate) => {
                        println!("Software update");
                        break;
                    }
                    _ => panic!("Unrecognized response code {r}"),
                },
            }
        }
        // Send GPS position
        match send_point(config.uid, channel.clone(), &config).await {
            Err(e) => {
                if s <= config.time.sleep_max_s {
                    eprintln!("Error: {e}");
                    eprintln!("Sleeping for {s}.{ms} s");
                    task::sleep(Duration::from_millis(s * 1000 + ms)).await;
                    s = std::cmp::min(s * 2, config.time.sleep_max_s);
                } else {
                    eprintln!("Error: {e}");
                    return Err(e);
                }
            }
            Ok(r) => match ResponseCode::from_i32(r) {
                Some(ResponseCode::CarryOn) => break,
                Some(ResponseCode::Exit) => return Ok(()),
                Some(ResponseCode::SoftwareUpdate) => {
                    println!("Software update");
                    break;
                }
                _ => panic!("Unrecognized response code {r}"),
            },
        }
    }
    let mut monitor_futures = vec![gpiomon(&config, lines[0], channel.clone())];
    let heartbeat_future = heartbeat(&config, channel.clone());

    for l in &lines[1..] {
        monitor_futures.push(gpiomon(&config, *l, channel.clone()));
    }

    match try_join(try_join_all(monitor_futures), heartbeat_future).await {
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
                Some(ResponseCode::Exit) => break,
                Some(ResponseCode::SoftwareUpdate) => {
                    println!("Software update");
                    match download().await {
                        Err(_) => {
                            eprintln!("Download failed. Let's continue as if nothing happened.")
                        }
                        Ok(_) => break,
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

async fn download() -> Result<(), std::io::Error> {
    let mut the_process = Command::new("curl")
        .arg("-O")
        .arg("https://hm.fps-gbg.net/files/ada/ada-client-new")
        .spawn()
        .ok()
        .expect("Failed to execute wget.");
    // Do things with `the_process`
    match the_process.wait() {
        Ok(_) => println!("Download completed"),
        Err(e) => return Err(e),
    }
    Ok(())
}
