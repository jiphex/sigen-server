use crate::{
    hardware::{Command, Job, SharedStatus},
    metrics,
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;
use std::{sync::mpsc, time::Duration};
use tokio::sync::oneshot;

#[derive(Clone)]
pub struct AppState {
    pub status: SharedStatus,
    pub commands: mpsc::SyncSender<Job>,
    pub allowed_hosts: Vec<String>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("../static/index.html")) }))
        .route("/app.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../static/app.js")) }))
        .route("/demo.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], include_str!("../static/demo.js")) }))
        .route("/style.css", get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], include_str!("../static/style.css")) }))
        .route("/api/status", get(status))
        .route("/api/control", post(control))
        .route("/api/alerts", post(update_alert))
        .route("/metrics", get(prometheus))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .layer(axum::middleware::from_fn(trace_request))
        .layer(axum::middleware::map_response(|mut response: Response| async move {
            let headers = response.headers_mut();
            headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
            headers.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
            headers.insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".parse().unwrap());
            response
        }))
        .with_state(state)
}
async fn trace_request(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    use tracing::Instrument;
    // Record only method and path, never headers, query strings or request bodies.
    let span = tracing::info_span!("http_request", method = %request.method(), path = request.uri().path());
    async move {
        let started = std::time::Instant::now();
        let response = next.run(request).await;
        let status = response.status().as_u16();
        let elapsed_ms = started.elapsed().as_millis() as u64;
        if response.status().is_server_error() {
            tracing::warn!(status, elapsed_ms, "HTTP request failed");
        } else {
            tracing::debug!(status, elapsed_ms, "HTTP request completed");
        }
        response
    }
    .instrument(span)
    .await
}

async fn status(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.status.read().expect("status lock").snapshot())
}
async fn prometheus(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics::render(&state.status.read().expect("status lock")),
    )
}

type ApiError = (StatusCode, Json<serde_json::Value>);
fn error(code: StatusCode, message: impl Into<String>) -> ApiError {
    (code, Json(json!({"error": message.into()})))
}

fn check_origin(headers: &HeaderMap, allowed_hosts: &[String]) -> Result<(), ApiError> {
    // Origin == Host alone permits DNS rebinding: an attacker's hostname can
    // resolve to this server while retaining a same-origin browser context.
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| error(StatusCode::FORBIDDEN, "Missing or invalid Host"))?;
    let authority = host
        .parse::<axum::http::uri::Authority>()
        .map_err(|_| error(StatusCode::FORBIDDEN, "Invalid Host"))?;
    let hostname = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host.contains('@')
        || !(hostname.eq_ignore_ascii_case("localhost")
            || hostname.parse::<std::net::IpAddr>().is_ok()
            || allowed_hosts
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(host)))
    {
        return Err(error(
            StatusCode::FORBIDDEN,
            "Untrusted Host; configure --allowed-host for this hostname and port",
        ));
    }
    // A custom header prevents cross-site form submissions. No CORS is enabled.
    if headers.get("x-sigen-control").and_then(|v| v.to_str().ok()) != Some("1") {
        return Err(error(
            StatusCode::FORBIDDEN,
            "Missing X-Sigen-Control header",
        ));
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin = origin.to_str().ok();
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if origin != Some(format!("http://{host}").as_str())
            && origin != Some(format!("https://{host}").as_str())
        {
            return Err(error(
                StatusCode::FORBIDDEN,
                "Cross-origin controls are not allowed",
            ));
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AlertUpdate {
    id: crate::alerts::Id,
    enabled: bool,
}

async fn update_alert(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(update): Json<AlertUpdate>,
) -> Result<impl IntoResponse, ApiError> {
    check_origin(&headers, &state.allowed_hosts)?;
    let alerts = tokio::task::spawn_blocking(move || {
        crate::alerts::set_enabled(&state.status, update.id, update.enabled)
    })
    .await
    .map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Alert settings update failed",
        )
    })?
    .map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The server could not save the alert settings. No changes were applied.",
        )
    })?;
    Ok(Json(alerts))
}

async fn control(
    State(state): State<AppState>,
    headers: HeaderMap,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    Json(command): Json<Command>,
) -> Result<Response, ApiError> {
    check_origin(&headers, &state.allowed_hosts)?;
    command
        .validate()
        .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    let (reply, response) = oneshot::channel();
    let job = Job::new(command, reply, &state.status.read().expect("status lock"))
        .map_err(|e| error(StatusCode::SERVICE_UNAVAILABLE, e))?
        .with_source(format!(
            "http POST /api/control peer={}",
            peer.map(|p| p.0.0.to_string())
                .unwrap_or_else(|| "unavailable".into())
        ));
    state.commands.try_send(job).map_err(|e| match e {
        mpsc::TrySendError::Full(_) => error(
            StatusCode::TOO_MANY_REQUESTS,
            "Control queue is full; no command was queued",
        ),
        mpsc::TrySendError::Disconnected(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Hardware worker unavailable",
        ),
    })?;
    match tokio::time::timeout(Duration::from_secs(60), response).await {
        Ok(Ok(Ok(controls))) => {
            Ok(Json(json!({"confirmed": true, "controls": controls})).into_response())
        }
        Ok(Ok(Err(e))) => Err(error(StatusCode::BAD_GATEWAY, e)),
        Ok(Err(_)) => Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Hardware worker stopped; check readback before retrying",
        )),
        Err(_) => Err(error(
            StatusCode::GATEWAY_TIMEOUT,
            "Control timed out; it may have executed. Check readback before retrying",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Controls, Status};
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use std::sync::{Arc, RwLock};
    use tower::ServiceExt;

    fn app(online: bool) -> (Router, mpsc::Receiver<Job>) {
        let mut status = Status::new("127.0.0.1:502".into(), 1);
        status.online = online;
        if online {
            status.control_valid_until = Some(std::time::Instant::now() + Duration::from_secs(45));
            status.controls = Some(Controls {
                remote_ems_enabled: true,
                remote_ems_mode: 2,
                backup_soc_percent: 20.0,
            });
        }
        let (commands, receiver) = mpsc::sync_channel(1);
        (
            router(AppState {
                status: Arc::new(RwLock::new(status)),
                commands,
                allowed_hosts: Vec::new(),
            }),
            receiver,
        )
    }
    fn control_request(body: &str, origin: Option<&str>, custom_header: bool) -> Request<Body> {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/control")
            .header("content-type", "application/json")
            .header("host", "localhost:3000");
        if custom_header {
            request = request.header("x-sigen-control", "1");
        }
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        request.body(Body::from(body.to_owned())).unwrap()
    }
    async fn body(response: Response) -> String {
        String::from_utf8(
            to_bytes(response.into_body(), 100_000)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn dashboard_assets_status_and_metrics_work_before_hardware_connects() {
        let (app, receiver) = app(false);
        for path in [
            "/",
            "/?demo=1",
            "/app.js",
            "/demo.js",
            "/style.css",
            "/api/status",
            "/metrics",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert!(
                response
                    .headers()
                    .contains_key(header::CONTENT_SECURITY_POLICY)
            );
            if path == "/metrics" {
                assert_eq!(
                    response.headers()[header::CONTENT_TYPE],
                    "text/plain; version=0.0.4; charset=utf-8"
                );
                let output = body(response).await;
                assert!(output.contains("# TYPE sigen_up gauge\nsigen_up 0\n"));
                assert!(!output.contains("sigen_grid_power_watts"));
            } else if path == "/api/status" {
                let status: serde_json::Value =
                    serde_json::from_str(&body(response).await).unwrap();
                assert_eq!(status["online"], false);
                assert!(status["readings"].is_null());
            }
        }
        assert!(receiver.try_recv().is_err()); // Status/scrapes never trigger Modbus traffic.
    }

    #[tokio::test]
    async fn controls_reject_cross_origin_invalid_and_offline_requests_without_queueing() {
        let (app, receiver) = app(false);
        let valid = r#"{"action":"backup_soc","percent":20}"#;
        for (req, code) in [
            (control_request(valid, None, false), StatusCode::FORBIDDEN),
            (
                control_request(valid, Some("https://other.example"), true),
                StatusCode::FORBIDDEN,
            ),
            (
                control_request(r#"{"action":"backup_soc","percent":101}"#, None, true),
                StatusCode::BAD_REQUEST,
            ),
            (
                control_request(r#"{"action":"ems_mode","mode":7}"#, None, true),
                StatusCode::BAD_REQUEST,
            ),
            (
                control_request(valid, Some("http://localhost:3000"), true),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ] {
            let response = app.clone().oneshot(req).await.unwrap();
            assert_eq!(response.status(), code);
        }
        assert!(receiver.try_recv().is_err());
        let response = app
            .oneshot(control_request(
                r#"{"action":"write_register","address":1}"#,
                None,
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn controls_wait_for_worker_confirmation_and_surface_errors() {
        let (app, receiver) = app(true);
        let worker = std::thread::spawn(move || {
            let first = receiver.recv().unwrap();
            assert!(first.source.starts_with("http POST /api/control peer="));
            assert!(matches!(
                first.command,
                Command::BackupSoc { percent: 20.0 }
            ));
            first
                .reply
                .send(Ok(Controls {
                    remote_ems_enabled: true,
                    remote_ems_mode: 2,
                    backup_soc_percent: 20.0,
                }))
                .unwrap();
            let second = receiver.recv().unwrap();
            second.reply.send(Err("readback differs".into())).unwrap();
        });
        let request = || control_request(r#"{"action":"backup_soc","percent":20}"#, None, true);
        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: serde_json::Value = serde_json::from_str(&body(response).await).unwrap();
        assert_eq!(value["confirmed"], true);
        assert_eq!(value["controls"]["backup_soc_percent"], 20.0);
        let response = app.oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(body(response).await.contains("readback differs"));
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn bounded_control_queue_rejects_overload() {
        let (app, _receiver) = app(true);
        // Keep the first handler waiting for confirmation, filling the single-slot test queue.
        let first_app = app.clone();
        let first = tokio::spawn(async move {
            first_app
                .oneshot(control_request(
                    r#"{"action":"remote_ems","enabled":true}"#,
                    None,
                    true,
                ))
                .await
        });
        tokio::task::yield_now().await;
        let response = app
            .oneshot(control_request(
                r#"{"action":"remote_ems","enabled":false}"#,
                None,
                true,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        first.abort();
    }
    #[tokio::test]
    async fn alert_toggles_work_offline_and_require_same_origin_requests() {
        let (app, receiver) = app(false);
        let request = |origin: Option<&str>, header: bool| {
            let mut request = control_request(
                r#"{"id":"grid_disconnected","enabled":false}"#,
                origin,
                header,
            );
            *request.uri_mut() = "/api/alerts".parse().unwrap();
            request
        };
        assert_eq!(
            app.clone()
                .oneshot(request(None, false))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.clone()
                .oneshot(request(Some("https://other.example"), true))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let response = app.clone().oneshot(request(None, true)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result: serde_json::Value = serde_json::from_str(&body(response).await).unwrap();
        let grid = result["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == "grid_disconnected")
            .unwrap();
        assert_eq!(grid["enabled"], false);
        let metrics = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            body(metrics)
                .await
                .contains("sigen_alert_enabled{alert=\"grid_disconnected\"} 0")
        );
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn same_origin_rebinding_hosts_are_rejected_unless_explicitly_trusted() {
        for host in [
            "attacker.example:3000",
            "localhost.attacker.example:3000",
            "localhost@attacker.example:3000",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, host.parse().unwrap());
            headers.insert(header::ORIGIN, format!("http://{host}").parse().unwrap());
            headers.insert("x-sigen-control", "1".parse().unwrap());
            assert!(check_origin(&headers, &[]).is_err());
        }
        for host in [
            "localhost:3000",
            "127.0.0.1:3000",
            "192.168.1.20:3000",
            "[::1]:3000",
            "sigen.example:8443",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, host.parse().unwrap());
            headers.insert(header::ORIGIN, format!("https://{host}").parse().unwrap());
            headers.insert("x-sigen-control", "1".parse().unwrap());
            assert!(check_origin(&headers, &["sigen.example:8443".into()]).is_ok());
        }
    }

    #[tokio::test]
    async fn stale_unconfirmed_and_stopping_hardware_rejects_controls_without_queueing() {
        for case in 0..3 {
            let mut s = Status::new("127.0.0.1:502".into(), 1);
            s.online = true;
            s.control_valid_until = Some(std::time::Instant::now() + Duration::from_secs(45));
            s.controls = Some(Controls {
                remote_ems_enabled: true,
                remote_ems_mode: 2,
                backup_soc_percent: 20.0,
            });
            match case {
                0 => s.control_valid_until = Some(std::time::Instant::now()),
                1 => s.controls = None,
                _ => s.shutting_down = true,
            }
            let (commands, receiver) = mpsc::sync_channel(1);
            let app = router(AppState {
                status: Arc::new(RwLock::new(s)),
                commands,
                allowed_hosts: Vec::new(),
            });
            let response = app
                .oneshot(control_request(
                    r#"{"action":"backup_soc","percent":20}"#,
                    None,
                    true,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert!(receiver.try_recv().is_err());
        }
    }
}
