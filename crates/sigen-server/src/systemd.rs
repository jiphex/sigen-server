//! Service-manager notifications are optional and Linux-only.

pub fn start(mut stop: tokio::sync::watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    #[cfg(target_os = "linux")]
    notify(sd_notify::NotifyState::Ready);

    tokio::spawn(async move {
        #[cfg(target_os = "linux")]
        let interval = sd_notify::watchdog_enabled().map(|timeout| {
            // Avoid a zero interval even for an unusually small watchdog timeout.
            (timeout / 2).max(std::time::Duration::from_nanos(1))
        });
        #[cfg(not(target_os = "linux"))]
        let interval: Option<std::time::Duration> = None;

        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                biased;
                result = stop.changed() => {
                    if result.is_err() || *stop.borrow() {
                        break;
                    }
                }
                _ = heartbeat(interval) => {
                    #[cfg(target_os = "linux")]
                    notify(sd_notify::NotifyState::Watchdog);
                }
            }
        }
        #[cfg(target_os = "linux")]
        notify(sd_notify::NotifyState::Stopping);
    })
}

async fn heartbeat(interval: Option<std::time::Duration>) {
    match interval {
        Some(interval) => tokio::time::sleep(interval).await,
        None => std::future::pending().await,
    }
}

#[cfg(target_os = "linux")]
fn notify(state: sd_notify::NotifyState<'_>) {
    if let Err(error) = sd_notify::notify(&[state]) {
        tracing::warn!(%error, "Failed to notify systemd");
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn notifier_stops_when_shutdown_is_requested() {
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let task = super::start(receiver);
        stop.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn notifier_stops_when_sender_is_dropped() {
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let task = super::start(receiver);
        drop(stop);
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }
}
