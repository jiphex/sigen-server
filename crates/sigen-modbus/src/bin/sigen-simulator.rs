#[path = "../simulator/mod.rs"]
mod simulator;

use std::{io, net::SocketAddr};

const HELP: &str = "Sigenergy Modbus TCP simulator (simulated hardware only)

Usage: sigen-simulator [OPTIONS]

  --bind IP:PORT       Listen address [default: 127.0.0.1:1502]
  --unit-id ID         Inverter unit ID, 1–246 [default: 1]
  --scenario NAME      solar, night, battery-alarm, thermal-runaway, grid-outage,
                       or cycle [default: solar]
  --cycle-seconds N    Seconds per scenario in cycle mode [default: 60]
  -h, --help           Print help
  -V, --version        Print version

Plant unit 247 and the chosen inverter share state across connections.
Both FC03 and FC04 reads are accepted. FC06/FC16 writes affect simulated state.
No real inverter is contacted. Stop with Ctrl-C.
";

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut address: SocketAddr = "127.0.0.1:1502".parse()?;
    let mut config = simulator::Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("sigen-simulator {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--bind" | "--unit-id" | "--scenario" | "--cycle-seconds" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for {flag}"))?;
                match flag.as_str() {
                    "--bind" => address = value.parse()?,
                    "--unit-id" => {
                        config.unit = sigen_modbus::UnitId::inverter(value.parse()?)?.get()
                    }
                    "--scenario" => config.scenario = value.parse()?,
                    _ => {
                        config.cycle_seconds = value.parse()?;
                        if config.cycle_seconds == 0 {
                            return Err("cycle seconds must be positive".into());
                        }
                    }
                }
            }
            _ => return Err(format!("unknown option {flag}; use --help").into()),
        }
    }
    let listener = std::net::TcpListener::bind(address)?;
    println!(
        "Sigen SIMULATOR listening on {} (inverter {}, plant 247, scenario {})",
        listener.local_addr()?,
        config.unit,
        config.scenario
    );
    // Flush readiness even when stdout is piped by a demo launcher or test.
    io::Write::flush(&mut io::stdout())?;
    simulator::serve(listener, config)?;
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Simulator failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
