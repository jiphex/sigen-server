use crate::{
    alerts::{Id, Severity},
    cli::PushoverArgs,
    hardware::{self, SharedStatus},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

const ENDPOINT: &str = "https://api.pushover.net/1/messages.json";

#[derive(Clone, Debug)]
pub struct Notification {
    pub id: Id,
    pub name: &'static str,
    pub active: bool,
    pub severity: Severity,
    pub timestamp: u64,
    pub target: String,
    pub unit: u8,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub sent_total: u64,
    pub failed_total: u64,
    pub last_sent_unix: Option<u64>,
    pub last_error: Option<String>,
}

// Never derive Debug or instrument arguments here: credentials are private.
pub struct Notifier {
    client: reqwest::Client,
    endpoint: String,
    token: String,
    user: String,
    device: Option<String>,
    recovery: bool,
}
#[derive(Deserialize)]
struct ApiResponse {
    status: u8,
}
#[derive(Debug)]
struct Failure {
    message: String,
    retry: bool,
}

impl Notifier {
    pub fn from_args(args: &PushoverArgs) -> Result<Option<Self>, String> {
        match (&args.pushover_token, &args.pushover_user) {
            (None, None) if args.pushover_device.is_none() => Ok(None),
            (Some(token), Some(user)) => {
                for (name, value) in [("token", token), ("user key", user)] {
                    if value.len() != 30 || !value.chars().all(|c| c.is_ascii_alphanumeric()) {
                        return Err(format!(
                            "Pushover {name} must contain exactly 30 letters or digits"
                        ));
                    }
                }
                if let Some(device) = &args.pushover_device
                    && (device.is_empty()
                        || device.len() > 25
                        || !device
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                {
                    return Err("Pushover device name must contain 1–25 letters, digits, underscores or hyphens".into());
                }
                Self::new(args, ENDPOINT).map(Some)
            }
            _ => Err("Pushover requires both an application token and a user/group key".into()),
        }
    }
    fn new(args: &PushoverArgs, endpoint: &str) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .https_only(endpoint.starts_with("https:"))
            .user_agent(concat!("sigen-server/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| "Could not initialise Pushover HTTPS client")?;
        Ok(Self {
            client,
            endpoint: endpoint.into(),
            token: args.pushover_token.clone().unwrap(),
            user: args.pushover_user.clone().unwrap(),
            device: args.pushover_device.clone(),
            recovery: args.pushover_recovery,
        })
    }
    /// Translate severity into delivery priority and classify failures for worker retries.
    /// Success means API acceptance, not acknowledgement by the notification recipient.
    async fn send(&self, note: &Notification) -> Result<(), Failure> {
        let title = format!(
            "Sigen: {}{}",
            note.name,
            if note.active { "" } else { " recovered" }
        );
        let message = format!(
            "{}: {}. Inverter {} at {}.",
            note.name,
            if note.active { "active" } else { "recovered" },
            note.unit,
            note.target
        );
        let priority = match (note.active, note.severity) {
            (false, _) => "0",
            (true, Severity::High) => "1",
            (true, Severity::Critical) => "2",
        };
        let mut fields = vec![
            ("token", self.token.clone()),
            ("user", self.user.clone()),
            ("title", title),
            ("message", message),
            ("timestamp", note.timestamp.to_string()),
            ("priority", priority.into()),
        ];
        if priority == "2" {
            fields.push(("retry", "60".into()));
            fields.push(("expire", "1800".into()));
        }
        if let Some(device) = &self.device {
            fields.push(("device", device.clone()));
        }
        let response = self
            .client
            .post(&self.endpoint)
            .form(&fields)
            .send()
            .await
            .map_err(|_| Failure {
                message: "Could not reach Pushover or request timed out".into(),
                retry: true,
            })?;
        let status = response.status();
        if status.as_u16() != 200 {
            return Err(Failure {
                message: format!("Pushover returned HTTP {}", status.as_u16()),
                retry: !status.is_client_error(),
            });
        }
        // Do not expose response bodies: an API error may echo submitted credentials.
        match response.json::<ApiResponse>().await {
            Ok(ApiResponse { status: 1 }) => Ok(()),
            Ok(_) => Err(Failure {
                message: "Pushover rejected the notification".into(),
                retry: false,
            }),
            Err(_) => Err(Failure {
                message: "Invalid or incomplete response from Pushover".into(),
                retry: true,
            }),
        }
    }
}

pub fn start(
    notifier: Notifier,
    status: SharedStatus,
    mut stop: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let (sender, mut receiver) = mpsc::channel::<Notification>(32);
    {
        let mut s = status.write().expect("status lock");
        s.pushover = Some(Status::default());
        s.alerts.notifications = Some(sender);
    }
    // Channel waits, HTTP delivery, and retry delays are async and yield Tokio workers.
    tokio::spawn(async move {
        tracing::info!(
            recovery = notifier.recovery,
            "Pushover notifications enabled"
        );
        loop {
            let note = tokio::select! { _ = stop.changed() => break, note = receiver.recv() => match note {Some(note)=>note,None=>break} };
            if !note.active && !notifier.recovery {
                continue;
            }
            // A toggle cancels queued deliveries as well as future transitions.
            if !enabled(&status, note.id) {
                continue;
            }
            let delivery = async {
                for attempt in 1..=3 {
                    if !enabled(&status, note.id) {
                        return;
                    }
                    match notifier.send(&note).await {
                        Ok(()) => {
                            let mut s = status.write().expect("status lock");
                            let p = s.pushover.as_mut().unwrap();
                            p.sent_total += 1;
                            p.last_sent_unix = Some(hardware::now());
                            p.last_error = None;
                            tracing::info!(
                                alert = note.id.key(),
                                recovery = !note.active,
                                "Pushover notification accepted"
                            );
                            return;
                        }
                        Err(error) => {
                            tracing::warn!(alert=note.id.key(),attempt,error=%error.message,"Pushover notification failed");
                            status
                                .write()
                                .expect("status lock")
                                .pushover
                                .as_mut()
                                .unwrap()
                                .last_error = Some(error.message);
                            if !error.retry || attempt == 3 {
                                status
                                    .write()
                                    .expect("status lock")
                                    .pushover
                                    .as_mut()
                                    .unwrap()
                                    .failed_total += 1;
                                return;
                            }
                            tokio::time::sleep(Duration::from_secs(5)).await;
                        }
                    }
                }
            };
            tokio::select! { _ = stop.changed() => break, _ = delivery => () }
        }
        tracing::info!("Pushover notification worker stopped");
    })
}
fn enabled(status: &SharedStatus, id: Id) -> bool {
    status
        .read()
        .expect("status lock")
        .alerts
        .items
        .iter()
        .any(|alert| alert.id == id && alert.enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Form, Router, extract::State, http::StatusCode, routing::post};
    use std::{
        collections::{HashMap, VecDeque},
        sync::{Arc, Mutex, RwLock},
    };
    fn args() -> PushoverArgs {
        PushoverArgs {
            pushover_token: Some("A".repeat(30)),
            pushover_user: Some("B".repeat(30)),
            pushover_device: Some("phone".into()),
            pushover_recovery: true,
        }
    }
    fn note(active: bool) -> Notification {
        Notification {
            id: Id::GridDisconnected,
            name: "Grid disconnected",
            active,
            severity: Severity::High,
            timestamp: 123,
            target: "127.0.0.1:502".into(),
            unit: 1,
        }
    }
    #[test]
    fn configuration_is_optional_and_validates_credentials_without_echoing_them() {
        assert!(
            Notifier::from_args(&PushoverArgs::default())
                .unwrap()
                .is_none()
        );
        assert!(Notifier::from_args(&args()).unwrap().is_some());
        let mut a = args();
        a.pushover_token = Some("secret-invalid-token".into());
        let error = Notifier::from_args(&a).err().unwrap();
        assert!(!error.contains("secret-invalid-token"));
        a = args();
        a.pushover_user = None;
        assert!(Notifier::from_args(&a).is_err());
        a = args();
        a.pushover_device = Some("bad device".into());
        assert!(Notifier::from_args(&a).is_err());
    }
    type MockState = Arc<Mutex<(VecDeque<(StatusCode, String)>, Vec<HashMap<String, String>>)>>;
    async fn request(
        State(state): State<MockState>,
        Form(fields): Form<HashMap<String, String>>,
    ) -> (StatusCode, String) {
        let mut s = state.lock().unwrap();
        s.1.push(fields);
        s.0.pop_front()
            .unwrap_or((StatusCode::OK, r#"{"status":1}"#.into()))
    }
    async fn mock(
        responses: Vec<(StatusCode, &str)>,
    ) -> (String, MockState, tokio::task::JoinHandle<()>) {
        let state = Arc::new(Mutex::new((
            responses.into_iter().map(|(s, b)| (s, b.into())).collect(),
            Vec::new(),
        )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/1/messages.json", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/1/messages.json", post(request))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, state, task)
    }
    #[tokio::test]
    async fn posts_form_and_classifies_success_permanent_and_transient_errors() {
        let (url, state, server) = mock(vec![
            (StatusCode::OK, r#"{"status":1}"#),
            (
                StatusCode::OK,
                r#"{"status":0,"errors":["secret credential"]}"#,
            ),
            (StatusCode::TOO_MANY_REQUESTS, r#"{"status":0}"#),
            (StatusCode::INTERNAL_SERVER_ERROR, "unavailable"),
        ])
        .await;
        let notifier = Notifier::new(&args(), &url).unwrap();
        notifier.send(&note(true)).await.unwrap();
        let invalid = notifier.send(&note(false)).await.unwrap_err();
        assert!(!invalid.retry);
        assert!(!invalid.message.contains("secret"));
        assert!(!notifier.send(&note(true)).await.unwrap_err().retry);
        assert!(notifier.send(&note(true)).await.unwrap_err().retry);
        let s = state.lock().unwrap();
        let form = &s.1[0];
        assert_eq!(form["token"], "A".repeat(30));
        assert_eq!(form["user"], "B".repeat(30));
        assert_eq!(form["device"], "phone");
        assert_eq!(form["priority"], "1");
        assert_eq!(form["timestamp"], "123");
        assert!(form["message"].contains("Inverter 1 at 127.0.0.1:502"));
        assert!(s.1[1]["title"].contains("recovered"));
        server.abort();
    }
    #[tokio::test]
    async fn priorities_and_emergency_repeat_parameters_follow_severity_and_recovery() {
        let (url, state, server) = mock(vec![]).await;
        let notifier = Notifier::new(&args(), &url).unwrap();
        for id in Id::ALL {
            let mut notification = note(true);
            notification.id = id;
            notifier.send(&notification).await.unwrap();
        }
        let mut critical = note(true);
        critical.severity = Severity::Critical;
        notifier.send(&critical).await.unwrap();
        critical.active = false;
        notifier.send(&critical).await.unwrap();
        let s = state.lock().unwrap();
        for form in &s.1[..Id::ALL.len()] {
            assert_eq!(form["priority"], "1");
            assert!(!form.contains_key("retry"));
            assert!(!form.contains_key("expire"));
        }
        assert_eq!(s.1[Id::ALL.len()]["priority"], "2");
        assert_eq!(s.1[Id::ALL.len()]["retry"], "60");
        assert_eq!(s.1[Id::ALL.len()]["expire"], "1800");
        assert_eq!(s.1[Id::ALL.len() + 1]["priority"], "0");
        assert!(!s.1[Id::ALL.len() + 1].contains_key("retry"));
        assert!(!s.1[Id::ALL.len() + 1].contains_key("expire"));
        server.abort();
    }
    #[tokio::test]
    async fn worker_sends_once_and_reports_acceptance() {
        let (url, state, server) = mock(vec![]).await;
        let notifier = Notifier::new(&args(), &url).unwrap();
        let shared = Arc::new(RwLock::new(crate::model::Status::new(
            "127.0.0.1:502".into(),
            1,
        )));
        let (stop, stopped) = watch::channel(false);
        let worker = start(notifier, shared.clone(), stopped);
        let sender = shared
            .read()
            .unwrap()
            .alerts
            .notifications
            .as_ref()
            .unwrap()
            .clone();
        sender.try_send(note(true)).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if shared.read().unwrap().pushover.as_ref().unwrap().sent_total == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stop.send(true).unwrap();
        worker.await.unwrap();
        assert_eq!(state.lock().unwrap().1.len(), 1);
        let s = shared.read().unwrap();
        assert!(s.pushover.as_ref().unwrap().last_sent_unix.is_some());
        assert_eq!(s.pushover.as_ref().unwrap().failed_total, 0);
        assert!(
            !serde_json::to_string(&*s)
                .unwrap()
                .contains(&"A".repeat(30))
        );
        server.abort();
    }
    #[tokio::test]
    async fn recovery_can_be_disabled_without_sending_any_request() {
        let (url, state, server) = mock(vec![]).await;
        let mut a = args();
        a.pushover_recovery = false;
        let shared = Arc::new(RwLock::new(crate::model::Status::new(
            "127.0.0.1:502".into(),
            1,
        )));
        let (stop, stopped) = watch::channel(false);
        let worker = start(Notifier::new(&a, &url).unwrap(), shared.clone(), stopped);
        shared
            .read()
            .unwrap()
            .alerts
            .notifications
            .as_ref()
            .unwrap()
            .try_send(note(false))
            .unwrap();
        // Send an activation after the recovery; its acceptance proves the queue was processed.
        shared
            .read()
            .unwrap()
            .alerts
            .notifications
            .as_ref()
            .unwrap()
            .try_send(note(true))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if shared.read().unwrap().pushover.as_ref().unwrap().sent_total == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stop.send(true).unwrap();
        worker.await.unwrap();
        assert_eq!(state.lock().unwrap().1.len(), 1);
        assert!(!state.lock().unwrap().1[0]["title"].contains("recovered"));
        server.abort();
    }
    #[tokio::test]
    async fn temporary_failures_retry_at_five_second_intervals_and_stop_after_three_attempts() {
        let (url, state, server) =
            mock(vec![(StatusCode::INTERNAL_SERVER_ERROR, "unavailable"); 3]).await;
        let shared = Arc::new(RwLock::new(crate::model::Status::new(
            "127.0.0.1:502".into(),
            1,
        )));
        let (stop, stopped) = watch::channel(false);
        let worker = start(
            Notifier::new(&args(), &url).unwrap(),
            shared.clone(),
            stopped,
        );
        let started = std::time::Instant::now();
        shared
            .read()
            .unwrap()
            .alerts
            .notifications
            .as_ref()
            .unwrap()
            .try_send(note(true))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if shared
                    .read()
                    .unwrap()
                    .pushover
                    .as_ref()
                    .unwrap()
                    .failed_total
                    == 1
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(started.elapsed() >= Duration::from_secs(10));
        stop.send(true).unwrap();
        worker.await.unwrap();
        assert_eq!(state.lock().unwrap().1.len(), 3);
        assert_eq!(
            shared.read().unwrap().pushover.as_ref().unwrap().sent_total,
            0
        );
        server.abort();
    }
}
