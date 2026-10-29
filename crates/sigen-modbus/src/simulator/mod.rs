mod model;
mod protocol;

use model::Model;
use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    Solar,
    Night,
    BatteryAlarm,
    ThermalRunaway,
    GridOutage,
    Cycle,
}
impl std::str::FromStr for Scenario {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "solar" => Ok(Self::Solar),
            "night" => Ok(Self::Night),
            "battery-alarm" => Ok(Self::BatteryAlarm),
            "thermal-runaway" => Ok(Self::ThermalRunaway),
            "grid-outage" => Ok(Self::GridOutage),
            "cycle" => Ok(Self::Cycle),
            _ => Err(
                "scenario must be solar, night, battery-alarm, thermal-runaway, grid-outage or cycle",
            ),
        }
    }
}
impl std::fmt::Display for Scenario {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Solar => "solar",
            Self::Night => "night",
            Self::BatteryAlarm => "battery-alarm",
            Self::ThermalRunaway => "thermal-runaway",
            Self::GridOutage => "grid-outage",
            Self::Cycle => "cycle",
        })
    }
}
pub struct Config {
    pub unit: u8,
    pub scenario: Scenario,
    pub cycle_seconds: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            unit: 1,
            scenario: Scenario::Solar,
            cycle_seconds: 60,
        }
    }
}

/// A small bounded threaded listener; all clients observe the same virtual plant.
pub fn serve(listener: TcpListener, config: Config) -> io::Result<()> {
    let model = Arc::new(Mutex::new(Model::new(config)));
    let clients = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = stream?;
        if clients.load(Ordering::Relaxed) >= 32 {
            continue;
        }
        clients.fetch_add(1, Ordering::Relaxed);
        let model = model.clone();
        let clients = clients.clone();
        thread::spawn(move || {
            if let Err(error) = connection(stream, &model)
                && !matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::BrokenPipe
                )
            {
                eprintln!("Simulator connection closed: {error}");
            }
            clients.fetch_sub(1, Ordering::Relaxed);
        });
    }
    Ok(())
}

fn connection(mut stream: TcpStream, model: &Mutex<Model>) -> io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    loop {
        // Idle clients may stay connected between slow polls, but incomplete frames
        // must finish within the read timeout and never allocate unbounded buffers.
        stream.set_read_timeout(None)?;
        let mut header = [0; 7];
        if stream.read(&mut header[..1])? == 0 {
            return Ok(());
        }
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.read_exact(&mut header[1..])?;
        let length = u16::from_be_bytes([header[4], header[5]]) as usize;
        if header[2..4] != [0, 0] || !(2..=254).contains(&length) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid MBAP header",
            ));
        }
        let mut request = vec![0; length - 1];
        stream.read_exact(&mut request)?;
        let response = protocol::respond(
            &mut model.lock().expect("simulator state"),
            header[6],
            &request,
        );
        // Broadcast writes are deliberately unsupported and never acknowledged.
        let Some(response) = response else {
            continue;
        };
        header[4..6].copy_from_slice(&((response.len() + 1) as u16).to_be_bytes());
        stream.write_all(&header)?;
        stream.write_all(&response)?;
    }
}
