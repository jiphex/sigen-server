mod alerts;
mod audit;
mod cli;
mod hardware;
mod metrics;
mod model;
mod mqtt;
mod pushover;
mod web;

use clap::Parser;
use sigen_modbus::{Config, ReadFunction, UnitId};
use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args = cli::Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    match run(args).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "Server failed");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(args: cli::Cli) -> Result<(), Box<dyn std::error::Error>> {
    let address = args.address;
    let bind = args.bind;
    let unit = UnitId::inverter(args.unit_id)?;
    let poll = args.poll_seconds;
    let telemetry_function = if args.read_function == 3 {
        ReadFunction::Holding
    } else {
        ReadFunction::Input
    };
    let notifier = pushover::Notifier::from_args(&args.pushover)?;
    let write_audit = Arc::new(audit::WriteAuditLog::open(&args.write_audit_file)?);
    let mqtt_config = mqtt::Config::from_args(&args.mqtt, &address.to_string(), unit.get(), poll)?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let status = Arc::new(RwLock::new(model::Status::new(
        address.to_string(),
        unit.get(),
    )));
    status.write().expect("status lock").alerts = alerts::Alerts::load(args.alerts_file, poll)?;
    write_audit.server_started(listener.local_addr()?, address, unit.get())?;
    status.write().expect("status lock").write_audit = Some(write_audit);
    // Register notification delivery before the first hardware poll can fail.
    let (stop, stop_receiver) = tokio::sync::watch::channel(false);
    let pushover_task =
        notifier.map(|notifier| pushover::start(notifier, status.clone(), stop_receiver.clone()));
    let commands = hardware::start(
        address,
        unit,
        Config {
            telemetry_function,
            ..Config::default()
        },
        Duration::from_secs(poll),
        status.clone(),
    );
    let alerts_task = alerts::start(status.clone(), stop_receiver.clone());
    let mqtt_task = mqtt_config
        .map(|config| mqtt::start(config, status.clone(), commands.clone(), stop_receiver));
    let app = web::router(web::AppState {
        status: status.clone(),
        commands,
        allowed_hosts: args.allowed_host,
    });
    tracing::info!(modbus_address = %address, unit_id = unit.get(), poll_seconds = poll, "Server started: http://{}", listener.local_addr()?);
    let shutdown_signal = stop.clone();
    let shutdown_status = status.clone();
    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown().await;
        tracing::info!("Shutdown requested");
        {
            let mut s = shutdown_status.write().expect("status lock");
            s.shutting_down = true;
            s.invalidate_controls();
        }
        let _ = shutdown_signal.send(true);
    })
    .await;
    {
        let mut s = status.write().expect("status lock");
        s.shutting_down = true;
        s.invalidate_controls();
    }
    let _ = stop.send(true);
    if let Some(task) = mqtt_task {
        let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
    }
    if let Some(task) = pushover_task {
        let _ = task.await;
    }
    let _ = alerts_task.await;
    result?;
    tracing::info!("Server stopped");
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => (), _ = terminate.recv() => () }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
