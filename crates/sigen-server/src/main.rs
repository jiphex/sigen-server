mod alerts;
mod audit;
mod cli;
mod hardware;
mod journal;
mod logging;
mod metrics;
mod model;
mod mqtt;
mod pushover;
mod telemetry;
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
    logging::init(args.log_stderr);
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
    let mut ac_units = args.ac_charger_units.clone();
    ac_units.sort_unstable();
    ac_units.dedup();
    if ac_units.len() > 4 || ac_units.contains(&args.unit_id) {
        return Err(
            "Configure at most four distinct AC charger units, different from the inverter unit"
                .into(),
        );
    }
    let read_only = !args.dangerously_enable_modbus_writes;
    let telemetry_function = if args.read_function == 3 {
        ReadFunction::Holding
    } else {
        ReadFunction::Input
    };
    let modbus_config = Config {
        read_only,
        telemetry_function,
        request_interval: Duration::from_millis(args.modbus_request_interval_ms),
        response_timeout: Duration::from_millis(args.modbus_response_timeout_ms),
        connect_timeout: Duration::from_millis(args.modbus_connect_timeout_ms),
    };
    let freshness_poll = poll + hardware::freshness_grace(&modbus_config).as_secs() - 30;
    let notifier = pushover::Notifier::from_args(&args.pushover)?;
    let write_audit = Arc::new(audit::WriteAuditLog::open(&args.write_audit_file)?);
    let mut mqtt_config =
        mqtt::Config::from_args(&args.mqtt, &address.to_string(), unit.get(), freshness_poll)?;
    if read_only {
        if let Some(config) = &mut mqtt_config {
            config.controls = false;
        }
        tracing::info!("Modbus read-only mode enabled; hardware controls disabled");
    } else {
        tracing::warn!(
            "DANGEROUS: Modbus writes enabled; controls can change whole-plant hardware settings"
        );
    }
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let status = Arc::new(RwLock::new(model::Status::new(
        address.to_string(),
        unit.get(),
    )));
    status.write().expect("status lock").alerts =
        alerts::Alerts::load(args.alerts_file, freshness_poll)?;
    status.write().expect("status lock").read_only = read_only;
    status.write().expect("status lock").ac_charger_units = ac_units;
    write_audit.server_started(listener.local_addr()?, address, unit.get())?;
    status.write().expect("status lock").write_audit = Some(write_audit);
    let journal = journal::Journal::open(&args.journal_file)?;
    {
        let mut s = status.write().expect("status lock");
        s.journal = Some(journal.clone());
        s.event(
            journal::Category::Server,
            "server_started",
            serde_json::json!({
                "version":env!("CARGO_PKG_VERSION"), "listen":listener.local_addr()?.to_string(),
                "write_audit_file":args.write_audit_file.to_string_lossy(),
                "read_only":read_only
            }),
        );
    }
    // Register notification delivery before the first hardware poll can fail.
    let (stop, stop_receiver) = tokio::sync::watch::channel(false);
    let pushover_task =
        notifier.map(|notifier| pushover::start(notifier, status.clone(), stop_receiver.clone()));
    let (commands, hardware_worker) = hardware::start(
        address,
        unit,
        modbus_config,
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
            s.event(
                journal::Category::Server,
                "shutdown_requested",
                serde_json::json!({}),
            );
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
    // Hardware can still be completing a blocking request when HTTP stops. Wait
    // for its terminal events before flushing the journal and recording shutdown.
    let hardware_result = tokio::task::spawn_blocking(move || hardware_worker.join()).await;
    let final_status = status.read().expect("status lock").snapshot();
    journal
        .finish(
            &final_status,
            if result.is_ok() && matches!(hardware_result, Ok(Ok(()))) {
                "server_stopped"
            } else {
                "server_failed"
            },
        )
        .await
        .map_err(std::io::Error::other)?;
    result?;
    if !matches!(hardware_result, Ok(Ok(()))) {
        return Err("Hardware worker failed during shutdown".into());
    }
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
