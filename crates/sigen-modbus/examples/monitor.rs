use sigen_modbus::{Client, UnitId};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    if let Err(error) = monitor() {
        tracing::error!(%error, "Monitoring failed");
        std::process::exit(1);
    }
    Ok(())
}

fn monitor() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let address = args
        .next()
        .ok_or("usage: monitor IP:PORT INVERTER_ID")?
        .parse()?;
    let unit = UnitId::inverter(args.next().ok_or("missing inverter ID")?.parse()?)?;
    let mut client = Client::connect(address)?;
    let device = client.read_device_info(unit)?;
    tracing::info!(device = ?device, "Device status");
    let plant = client.read_plant()?;
    tracing::info!(plant = ?plant, "Plant status");
    let inverter = client.read_inverter(unit)?;
    tracing::info!(inverter = ?inverter, "Inverter status");
    let battery = client.read_battery(unit)?;
    tracing::info!(battery = ?battery, "Battery status");
    Ok(())
}
