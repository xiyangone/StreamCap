//! Replays lifecycle races through real loopback HTTP and the native FLV recorder.
//! All state and media are confined to this test's temporary directory.
use axum::{body::Body, routing::get, Router};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{sync::Arc, time::Duration};
use streamcap_core::{
    api::{self, ApiState},
    resolver::StreamInfo,
    service::{Server, ServerOptions},
    Recording, Workspace,
};

struct Fixture {
    _directory: tempfile::TempDir,
    workspace: Workspace,
    state: ApiState,
    server: Server,
    source: tokio::task::JoinHandle<()>,
    info: StreamInfo,
    base: String,
    client: reqwest::Client,
    requests: Arc<AtomicUsize>,
}

#[tokio::test]
async fn flv_redirect_follows_cdn_without_forwarding_custom_credentials() {
    use axum::http::HeaderMap;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination = listener.local_addr().unwrap();
    let target = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/media",
                get(|headers: HeaderMap| async move {
                    assert!(headers.get("cookie").is_none());
                    assert!(headers.get("authorization").is_none());
                    assert!(headers.get("x-api-key").is_none());
                    b"FLV\x01\x05\x00\x00\x00\x09\x00\x00\x00\x00payload".to_vec()
                }),
            ),
        )
        .await
        .unwrap();
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source = listener.local_addr().unwrap();
    let redirect = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/redirect",
                get(move || async move {
                    axum::response::Redirect::temporary(&format!("http://{destination}/media"))
                }),
            ),
        )
        .await
        .unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("redirect.flv");
    let tracker = tokio_util::task::TaskTracker::new();
    let download = streamcap_core::direct::Download::start(
        &path,
        &format!("http://{source}/redirect"),
        None,
        Some("Cookie: sid=secret\r\nAuthorization: Bearer secret\r\nX-API-Key: secret\r\n"),
        &tracker,
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), download.done.cancelled())
        .await
        .unwrap();
    assert!(download.result.lock().await.as_ref().unwrap().is_ok());
    assert!(std::fs::read(&path).unwrap().starts_with(b"FLV"));
    tracker.close();
    tracker.wait().await;
    target.abort();
    redirect.abort();
    let _ = target.await;
    let _ = redirect.await;
}

#[tokio::test]
#[ignore = "requires explicit STREAMCAP_REAL_PROFILE, STREAMCAP_REAL_ROOM_INDICES and STREAMCAP_TEST_FFMPEG; reads live rooms without login or recording"]
async fn real_rooms_resolve_and_preview_without_changing_user_data() {
    use futures::StreamExt;
    use std::path::PathBuf;
    use streamcap_core::resolver::{ResolveRequest, Resolver};

    let profile =
        PathBuf::from(std::env::var("STREAMCAP_REAL_PROFILE").expect("explicit profile required"));
    assert!(profile.is_absolute());
    let config = profile.join("config");
    let names = ["recordings.json", "user_settings.json", "cookies.json"];
    let before: Vec<_> = names
        .iter()
        .map(|name| std::fs::read(config.join(name)).unwrap())
        .collect();
    let records: Vec<Value> = serde_json::from_slice(&before[0]).unwrap();
    let settings: Value = serde_json::from_slice(&before[1]).unwrap();
    let cookies: Value = serde_json::from_slice(&before[2]).unwrap();
    assert_ne!(
        settings["enable_proxy"], true,
        "this probe must not silently bypass an enabled application proxy"
    );
    let indices: Vec<usize> = std::env::var("STREAMCAP_REAL_ROOM_INDICES")
        .expect("explicit room indices required")
        .split(',')
        .map(|n| n.parse().unwrap())
        .collect();
    assert!(!indices.is_empty() && indices.len() <= 7);
    let mut seen = std::collections::HashSet::new();
    for index in &indices {
        let record = &records[*index];
        assert_eq!(
            record["monitor_status"], true,
            "do not probe paused user rooms"
        );
        let (_, platform) =
            streamcap_core::model::detect_platform(record["url"].as_str().unwrap()).unwrap();
        assert!(matches!(platform.as_str(), "douyin" | "kuaishou"));
        seen.insert(platform);
    }
    let ffmpeg =
        PathBuf::from(std::env::var("STREAMCAP_TEST_FFMPEG").expect("explicit FFmpeg required"));
    assert!(ffmpeg.is_file());
    let observer_stop = tokio_util::sync::CancellationToken::new();
    let observer_cancel = observer_stop.clone();
    let _observer_guard = observer_stop.clone().drop_guard();
    let observer = tokio::spawn(async move {
        let mut observed = Vec::<Value>::new();
        let Ok(path) = std::env::var("STREAMCAP_PROXY_CONFIG") else {
            return observed;
        };
        let Ok(bytes) = std::fs::read(path) else {
            return observed;
        };
        let Ok(config) = serde_json::from_slice::<Value>(&bytes) else {
            return observed;
        };
        let Some(address) = config
            .pointer("/experimental/clash_api/external_controller")
            .and_then(Value::as_str)
        else {
            return observed;
        };
        if !address.starts_with("127.0.0.1:") {
            return observed;
        }
        let secret = config
            .pointer("/experimental/clash_api/secret")
            .and_then(Value::as_str)
            .unwrap_or("");
        let endpoint = format!("http://{address}/connections");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let exe = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .to_lowercase();
        loop {
            let request = client.get(&endpoint);
            let request = if secret.is_empty() {
                request
            } else {
                request.bearer_auth(secret)
            };
            if let Ok(response) = request.send().await {
                if let Ok(data) = response.json::<Value>().await {
                    for connection in data["connections"].as_array().into_iter().flatten() {
                        let metadata = &connection["metadata"];
                        let process = metadata["processPath"]
                            .as_str()
                            .unwrap_or("")
                            .to_lowercase();
                        let host = metadata["host"].as_str().unwrap_or("");
                        if process != exe
                            || !(host.ends_with("douyin.com") || host.ends_with("kuaishou.com"))
                        {
                            continue;
                        }
                        let row = json!({"host":host,"chains":connection["chains"],"rule":connection["rule"],"rulePayload":connection["rulePayload"],"processMatched":true});
                        if !observed.contains(&row) {
                            observed.push(row);
                        }
                    }
                }
            }
            tokio::select! { _=observer_cancel.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(100))=>{} }
        }
        observed
    });
    let resolver = Resolver::new();
    let pacer = streamcap_core::pacing::Pacer::default();
    let maximum = settings["platform_max_concurrent_requests"]
        .as_str()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let spacing = settings["platform_request_interval"]
        .as_str()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let mut reports = Vec::new();
    let mut blocked = std::collections::HashSet::new();
    for index in indices {
        let record = &records[index];
        let url = record["url"].as_str().unwrap();
        let (_, platform) = streamcap_core::model::detect_platform(url).unwrap();
        if blocked.contains(&platform) {
            println!(
                "REAL_ROOM_SKIPPED platform={} reason=access_restriction",
                platform
            );
            continue;
        }
        let cookie = cookies[&platform]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned);
        let started = std::time::Instant::now();
        let permit = pacer
            .acquire(
                &platform,
                maximum,
                Duration::from_secs(spacing),
                &resolver.cancellation(),
            )
            .await
            .unwrap();
        let result = resolver
            .resolve(ResolveRequest {
                account: None,
                url: url.into(),
                quality: Some(
                    record["quality"]
                        .as_str()
                        .or(settings["record_quality"].as_str())
                        .unwrap_or("OD")
                        .into(),
                ),
                proxy: None,
                cookie: cookie.clone(),
                platform: Some(platform.clone()),
            })
            .await;
        pacer.result(&platform, &result);
        drop(permit);
        let mut report = json!({"index":index,"platform":platform,"resolveMilliseconds":started.elapsed().as_millis(),
            "cookieProvided":cookie.is_some(),"authentication":"not independently verified","mediaBytes":0});
        match result {
            Ok(info) => {
                report["transport"] = json!("reachable");
                report["businessReadable"] = json!(true);
                report["live"] = json!(info.is_live);
                report["targetDataPresent"] = json!(!info.anchor_name.is_empty());
                if info.is_live {
                    let prefer_flv = settings["default_live_source"].as_str() != Some("HLS");
                    let source = if !prefer_flv && !info.m3u8_url.is_empty() {
                        Some(info.m3u8_url.clone())
                    } else {
                        info.pick_record_url(prefer_flv)
                    };
                    if let Some(mut source) = source {
                        if settings["force_https_recording"] == true
                            && source.starts_with("http://")
                        {
                            source = source.replacen("http://", "https://", 1);
                        }
                        let preview = streamcap_core::preview::Preview::new(
                            streamcap_core::Engine::new(),
                            tokio_util::sync::CancellationToken::new(),
                        );
                        let referer = match platform.as_str() {
                            "douyin" => "https://live.douyin.com/",
                            "kuaishou" => "https://live.kuaishou.com/",
                            _ => unreachable!(),
                        };
                        let mut bytes = Vec::new();
                        let media_start = std::time::Instant::now();
                        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
                            let response = preview
                                .live(
                                    ffmpeg.clone(),
                                    streamcap_core::preview::LiveInput {
                                        url: source,
                                        headers: Some(format!("Referer: {referer}\r\n")),
                                        proxy: None,
                                    },
                                )
                                .await
                                .map_err(|_| "preview startup failed")?;
                            let mut stream = response.into_body().into_data_stream();
                            while let Some(chunk) = stream.next().await {
                                let chunk = chunk.map_err(|_| "preview stream failed")?;
                                let count = chunk.len().min(512 * 1024 - bytes.len());
                                bytes.extend_from_slice(&chunk[..count]);
                                if bytes.len() >= 512 * 1024 {
                                    break;
                                }
                            }
                            Ok::<(), &str>(())
                        })
                        .await;
                        report["mediaMilliseconds"] = json!(media_start.elapsed().as_millis());
                        report["mediaBytes"] = json!(bytes.len());
                        report["mediaDeadlineReached"] = json!(outcome.is_err());
                        report["mediaStreamError"] = json!(matches!(outcome, Ok(Err(_))));
                        report["mpegTsReadable"] = json!(
                            bytes.len() >= 188 * 10
                                && bytes
                                    .chunks_exact(188)
                                    .take(10)
                                    .all(|packet| packet[0] == 0x47)
                        );
                        preview.shutdown().await;
                        assert_eq!(preview.active(), 0, "real preview must release its worker");
                        if report["mpegTsReadable"] == true && !info.flv_url.is_empty() {
                            report["controlledRecovery"] =
                                verify_live_flv_recovery(&info.flv_url, referer).await;
                        }
                    }
                }
            }
            Err(error) => {
                if [
                    "HTTP 429",
                    "HTTP 403",
                    "400002",
                    "验证码",
                    "滑块",
                    "登录",
                    "冷却",
                ]
                .iter()
                .any(|marker| error.contains(marker))
                {
                    blocked.insert(platform.clone());
                }
                report["businessReadable"] = json!(false);
                report["live"] = Value::Null;
                report["targetDataPresent"] = Value::Null;
                report["transport"] = json!(if error.contains("超时")
                    || error.contains("连接")
                    || error.contains("TLS")
                {
                    "not confirmed"
                } else {
                    "response received"
                });
                report["accessState"] =
                    json!(streamcap_core::platforms::kuaishou::access_state(&error));
                let safe_error = if error.contains("://")
                    || cookie.as_ref().is_some_and(|c| error.contains(c.as_str()))
                {
                    "platform error details withheld".to_string()
                } else {
                    error.chars().take(240).collect()
                };
                report["error"] = json!(safe_error);
            }
        }
        println!("REAL_ROOM_PROBE {}", report);
        reports.push(report);
    }
    resolver.shutdown().await;
    observer_stop.cancel();
    for row in observer.await.unwrap() {
        println!("REAL_ROUTE_PROBE {}", row);
    }
    for (name, expected) in names.iter().zip(before.iter()) {
        assert!(
            &std::fs::read(config.join(name)).unwrap() == expected,
            "user file {name} changed"
        );
    }
    assert!(!reports.is_empty() && reports.len() <= 7);
    assert!(!seen.is_empty());
    println!("REAL_ROOM_SAFETY user_files_unchanged=true no_login=true no_recording=true");
}

// Break only the first bounded relay response; the real source and original task are untouched.
async fn verify_live_flv_recovery(source: &str, referer: &str) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let state = api::bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state.config.write().await.update_user_config(json!({"loop_time_seconds":"4500","platform_request_interval":"3","platform_max_concurrent_requests":"1","recording_space_threshold":"0.1","convert_to_mp4":false,"system_notification_enabled":false,"stream_start_notification_enabled":false,"stream_end_notification_enabled":false,"execute_custom_script":false}).as_object().unwrap().clone()).unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let url = source.to_string();
    let referer = referer.to_string();
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let app = Router::new().route(
        "/real.flv",
        get(move || {
            let client = client.clone();
            let url = url.clone();
            let referer = referer.clone();
            let observed = observed.clone();
            async move {
                let index = observed.fetch_add(1, Ordering::SeqCst);
                if index >= 2 {
                    return axum::http::Response::builder()
                        .status(503)
                        .body(Body::empty())
                        .unwrap();
                }
                let Ok(response) = client
                    .get(url)
                    .header("Referer", referer)
                    .header("User-Agent", streamcap_core::engine::FFMPEG_USER_AGENT)
                    .send()
                    .await
                else {
                    return axum::http::Response::builder()
                        .status(502)
                        .body(Body::empty())
                        .unwrap();
                };
                if !response.status().is_success() {
                    return axum::http::Response::builder()
                        .status(502)
                        .body(Body::empty())
                        .unwrap();
                }
                let stream = futures::stream::try_unfold(
                    (response, 0usize),
                    move |(mut response, count)| async move {
                        if index == 0 && count >= 128 * 1024 {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "controlled first relay interruption",
                            ));
                        }
                        if count >= 4 * 1024 * 1024 {
                            tokio::time::sleep(Duration::from_secs(20)).await;
                            return Ok(None);
                        }
                        match response.chunk().await {
                            Ok(Some(chunk)) => {
                                let next = count + chunk.len();
                                Ok(Some((chunk, (response, next))))
                            }
                            Ok(None) => Ok(None),
                            Err(_) => Err(std::io::Error::other("source transport ended")),
                        }
                    },
                );
                axum::http::Response::builder()
                    .header("Content-Type", "video/x-flv")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/real.flv", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut record = Recording::new("real-recovery".into(), url, "Real recovery sample".into());
    record.record_format = Some("FLV".into());
    record.flv_use_direct_download = Some(true);
    record.segment_record = Some(false);
    record.enabled_message_push = Some(false);
    state.store.insert(vec![record]).await.unwrap();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let scheduler = tokio::spawn(state.scheduler.clone().run(rx));
    let result = tokio::time::timeout(Duration::from_secs(35), async {
        loop {
            let record = state.store.get("real-recovery").await.unwrap();
            if requests.load(Ordering::SeqCst) >= 2
                && record.is_recording
                && record.recovery_attempt > 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    state.scheduler.stop_recording("real-recovery").await;
    let count = requests.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(6)).await;
    let stopped = requests.load(Ordering::SeqCst) == count;
    let _ = stop.send(true);
    scheduler.await.unwrap();
    api::shutdown(&state).await.unwrap();
    server.abort();
    let _ = server.await;
    json!({"method":"real FLV via bounded loopback relay; only first response interrupted","recovered":result.is_ok(),"requests":count,"stopPreventedRestart":stopped,"originalTaskUntouched":true})
}

impl Fixture {
    async fn new(monitor: bool) -> Self {
        Self::with_failures(monitor, 0).await
    }
    async fn with_failures(monitor: bool, failures: usize) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::from_repo_root(directory.path());
        let state = api::bootstrap(workspace.clone()).await.unwrap();
        state
            .config
            .write()
            .await
            .update_user_config(
                json!({
                    "live_save_path": directory.path().join("downloads"),
                    "recording_space_threshold":"0.1", "loop_time_seconds":"4500",
                    "convert_to_mp4":false, "generate_time_subtitle_file":false,
                    "execute_custom_script":false, "system_notification_enabled":false
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_url = format!("http://{}/source.flv", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let stream = Router::new().route(
            "/source.flv",
            get(move || {
                let observed = observed.clone();
                async move {
                    if observed.fetch_add(1, Ordering::SeqCst) < failures {
                        return axum::http::Response::builder()
                            .status(503)
                            .body(Body::empty())
                            .unwrap();
                    }
                    axum::http::Response::new(Body::from_stream(futures::stream::unfold(
                        0usize,
                        |index| async move {
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            let mut bytes = vec![0u8; 4096];
                            if index == 0 {
                                bytes[..9].copy_from_slice(&[b'F', b'L', b'V', 1, 5, 0, 0, 0, 9]);
                            }
                            Some((Ok::<_, std::io::Error>(bytes), index + 1))
                        },
                    )))
                }
            }),
        );
        let source = tokio::spawn(async move {
            axum::serve(listener, stream).await.unwrap();
        });
        let mut record = Recording::new("race".into(), source_url.clone(), "Fixture".into());
        record.monitor_status = monitor;
        record.record_format = Some("FLV".into());
        record.segment_record = Some(false);
        record.flv_use_direct_download = Some(true);
        state.store.insert(vec![record]).await.unwrap();
        let info = StreamInfo {
            anchor_name: "Fixture".into(),
            is_live: true,
            flv_url: source_url,
            ..Default::default()
        };
        let server = Server::start(
            state.clone(),
            ServerOptions {
                port: 0,
                monitoring: false,
            },
        )
        .await
        .unwrap();
        let base = format!("http://{}", server.address());
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(8))
            .build()
            .unwrap();
        Self {
            _directory: directory,
            workspace,
            state,
            server,
            source,
            info,
            base,
            client,
            requests,
        }
    }
    async fn finish(self) {
        self.server.shutdown().await.unwrap();
        assert!(self.state.engine.active_ids().await.is_empty());
        self.source.abort();
        let _ = self.source.await;
    }
    fn mutation(&self, action: &str) -> reqwest::RequestBuilder {
        match action {
            "delete" => self
                .client
                .delete(format!("{}/api/recordings/race", self.base)),
            "delete-batch" => self
                .client
                .post(format!("{}/api/recordings/delete", self.base))
                .json(&json!({"recIds":["race"]})),
            "edit" => self
                .client
                .put(format!("{}/api/recordings/race", self.base))
                .json(&json!({"changes":{"streamerName":"Changed"}})),
            "edit-batch" => self
                .client
                .post(format!("{}/api/recordings/batch-edit", self.base))
                .json(&json!({"recIds":["race"],"changes":{"recordFormat":"MP4"}})),
            _ => unreachable!(),
        }
    }
    async fn written_bytes(&self) -> u64 {
        let process = self.state.engine.process("race").await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let length = std::fs::metadata(&process.output_path)
                    .map(|value| value.len())
                    .unwrap_or(0);
                if length > 13 {
                    return length;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn start_delete_and_edit_are_serialized_until_runtime_state_is_published() {
    for action in ["delete", "delete-batch", "edit", "edit-batch"] {
        let fixture = Fixture::new(true).await;
        let filesystem = fixture.state.engine.filesystem_guard().await;
        let mut start = Box::pin(
            fixture
                .state
                .scheduler
                .start_recording("race".into(), &fixture.info),
        );
        assert!(futures::poll!(start.as_mut()).is_pending());
        assert!(!fixture.state.store.get("race").await.unwrap().is_recording);
        assert!(!fixture.state.engine.is_recording("race").await);
        let request = fixture.mutation(action);
        let mut mutation = tokio::spawn(async move { request.send().await.unwrap() });
        assert!(
            tokio::time::timeout(Duration::from_millis(120), &mut mutation)
                .await
                .is_err(),
            "{action} crossed an in-flight start"
        );
        drop(filesystem);
        tokio::time::timeout(Duration::from_secs(3), start)
            .await
            .unwrap()
            .unwrap();
        let response = mutation.await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT, "{action}");
        let record = fixture.state.store.get("race").await.unwrap();
        assert!(record.is_recording && fixture.state.engine.is_recording("race").await);
        assert_eq!(record.streamer_name, "Fixture");
        assert_eq!(record.record_format.as_deref(), Some("FLV"));
        assert!(fixture.written_bytes().await > 13);
        let output = fixture
            .state
            .engine
            .process("race")
            .await
            .unwrap()
            .output_path
            .clone();
        let response = fixture
            .client
            .post(format!("{}/api/recordings/race/stop", fixture.base))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert!(fixture
            .mutation(action)
            .send()
            .await
            .unwrap()
            .status()
            .is_success());
        assert!(
            output.exists(),
            "task edits/deletion never remove recorded media"
        );
        fixture.finish().await;
    }
}

#[tokio::test]
async fn deleted_task_cannot_be_started_with_an_old_resolved_source() {
    let fixture = Fixture::new(true).await;
    assert_eq!(
        fixture.state.store.remove(&["race".into()]).await.unwrap(),
        1
    );
    assert!(fixture
        .state
        .scheduler
        .start_recording("race".into(), &fixture.info)
        .await
        .is_err());
    assert!(fixture.state.engine.active_ids().await.is_empty());
    fixture.finish().await;
}

#[cfg(windows)]
#[tokio::test]
async fn failed_monitor_persistence_does_not_publish_or_stop_recording() {
    use std::os::windows::fs::OpenOptionsExt;
    for monitored in [false, true] {
        let fixture = Fixture::new(monitored).await;
        if monitored {
            fixture
                .state
                .scheduler
                .start_recording("race".into(), &fixture.info)
                .await
                .unwrap();
            fixture.written_bytes().await;
        }
        let before = std::fs::read(fixture.workspace.recordings_path()).unwrap();
        let memory = fixture.state.store.get("race").await.unwrap();
        let mut events = fixture.state.store.subscribe();
        // Allow reads/writes but deny replacing the file, matching Windows sharing failures.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .open(fixture.workspace.recordings_path())
            .unwrap();
        let response = fixture
            .client
            .post(format!("{}/api/recordings/race/monitor", fixture.base))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(fixture.state.store.get("race").await.unwrap(), memory);
        assert_eq!(
            std::fs::read(fixture.workspace.recordings_path()).unwrap(),
            before
        );
        assert!(
            events.try_recv().is_err(),
            "failed transition emitted an event"
        );
        assert_eq!(fixture.state.engine.is_recording("race").await, monitored);
        drop(lock);
        let response = fixture
            .client
            .post(format!("{}/api/recordings/race/monitor", fixture.base))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let updated: Value = response.json().await.unwrap();
        assert_eq!(updated["monitorStatus"], !monitored);
        let reloaded = streamcap_core::store::Store::new(fixture.workspace.clone());
        reloaded.load().await.unwrap();
        assert_eq!(
            reloaded.get("race").await.unwrap().monitor_status,
            !monitored
        );
        fixture.finish().await;
    }
}

#[tokio::test]
async fn concurrent_monitor_toggles_do_not_lose_transitions() {
    let fixture = Fixture::new(false).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(12));
    let mut toggles = Vec::new();
    for _ in 0..12 {
        let request = fixture
            .client
            .post(format!("{}/api/recordings/race/monitor", fixture.base));
        let barrier = barrier.clone();
        toggles.push(tokio::spawn(async move {
            barrier.wait().await;
            let response = request.send().await.unwrap();
            assert!(response.status().is_success());
            response.json::<Value>().await.unwrap()["monitorStatus"]
                .as_bool()
                .unwrap()
        }));
    }
    let mut enabled = 0;
    for toggle in toggles {
        enabled += usize::from(toggle.await.unwrap());
    }
    assert_eq!(enabled, 6);
    assert!(
        !fixture
            .state
            .store
            .get("race")
            .await
            .unwrap()
            .monitor_status
    );
    let reloaded = streamcap_core::store::Store::new(fixture.workspace.clone());
    reloaded.load().await.unwrap();
    assert!(!reloaded.get("race").await.unwrap().monitor_status);
    fixture.finish().await;
}

#[tokio::test]
async fn preview_lists_only_immediate_final_media_files() {
    let fixture = Fixture::new(false).await;
    let root = fixture.state.config.read().await.recordings_root();
    let folder = root.join("preview");
    std::fs::create_dir_all(folder.join("nested.ts/deeper")).unwrap();
    for name in [
        "valid.mp4",
        "next.ts",
        "notes.txt",
        ".streamcap-remux-working.mp4",
        ".streamcap-subtitle-working.ts",
        "nested.ts/deeper/unrelated.mp4",
    ] {
        std::fs::write(folder.join(name), b"fixture").unwrap();
    }
    fixture
        .state
        .store
        .update("race", |record| {
            record.recording_dir = Some(folder.to_string_lossy().into_owned())
        })
        .await;
    let response = fixture
        .client
        .get(format!("{}/api/recordings/race/files", fixture.base))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let result: Value = response.json().await.unwrap();
    let mut paths = result["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(paths, ["preview/next.ts", "preview/valid.mp4"]);
    fixture.finish().await;
}

#[tokio::test]
async fn interrupted_stream_recovers_without_waiting_4500_seconds_and_stop_cancels_recovery() {
    let fixture = Fixture::with_failures(true, 1).await;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let scheduler = tokio::spawn(fixture.state.scheduler.clone().run(rx));
    tokio::time::timeout(Duration::from_secs(4), async {
        while fixture
            .state
            .store
            .get("race")
            .await
            .unwrap()
            .recovery_at
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let interrupted = fixture.state.store.get("race").await.unwrap();
    assert_eq!(interrupted.recovery_attempt, 1);
    assert!(!interrupted.is_recording);
    assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
    tokio::time::timeout(Duration::from_secs(8), async {
        while fixture.requests.load(Ordering::SeqCst) < 2
            || !fixture.state.store.get("race").await.unwrap().is_recording
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(fixture.written_bytes().await > 13);
    let response = fixture
        .client
        .post(format!("{}/api/recordings/race/stop", fixture.base))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let stopped = fixture.state.store.get("race").await.unwrap();
    assert!(!stopped.is_recording && stopped.monitor_status);
    assert!(stopped.recovery_at.is_none() && stopped.queue_position.is_none());
    assert_eq!(stopped.recovery_attempt, 0);
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(
        fixture.requests.load(Ordering::SeqCst),
        2,
        "manual stop must not trigger a recovery restart"
    );
    assert_eq!(
        fixture
            .state
            .config
            .read()
            .await
            .get_i64("loop_time_seconds", 0),
        4500
    );
    stop.send(true).unwrap();
    scheduler.await.unwrap();
    fixture.finish().await;
}

#[tokio::test]
async fn committed_room_change_and_monitor_resume_start_without_manual_detection() {
    for action in ["room", "resume"] {
        let fixture = Fixture::new(action == "room").await;
        fixture
            .state
            .store
            .update("race", |r| {
                if action == "room" {
                    r.url = "http://127.0.0.1:1/old.flv".into();
                    r.only_notify_no_record = Some(true);
                }
            })
            .await;
        let (stop, rx) = tokio::sync::watch::channel(false);
        let scheduler = tokio::spawn(fixture.state.scheduler.clone().run(rx));
        if action == "room" {
            tokio::time::timeout(Duration::from_secs(3), async {
                while fixture
                    .state
                    .store
                    .get("race")
                    .await
                    .unwrap()
                    .last_check_at
                    .is_none()
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
        let response = if action == "resume" {
            fixture
                .client
                .post(format!("{}/api/recordings/race/monitor", fixture.base))
        } else {
            fixture
                .client
                .put(format!("{}/api/recordings/race", fixture.base))
                .json(&json!({"changes":{"url":fixture.info.flv_url,"onlyNotifyNoRecord":false}}))
        }
        .send()
        .await
        .unwrap();
        assert!(response.status().is_success());
        let started = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture.state.store.get("race").await.unwrap().is_recording
                    && fixture.requests.load(Ordering::SeqCst) > 0
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .is_ok();
        if started {
            assert!(fixture.written_bytes().await > 13);
        }
        assert_eq!(
            fixture
                .state
                .config
                .read()
                .await
                .get_i64("loop_time_seconds", 0),
            4500
        );
        stop.send(true).unwrap();
        scheduler.await.unwrap();
        fixture.finish().await;
        assert!(
            started,
            "{action}: committed monitor intent must not wait 4500 seconds"
        );
    }
}

#[tokio::test]
async fn user_paused_verification_never_starts_a_live_recording() {
    let directory = tempfile::tempdir().unwrap();
    let state = api::bootstrap(Workspace::from_repo_root(directory.path()))
        .await
        .unwrap();
    let mut record = Recording::new(
        "paused-page".into(),
        "https://live.kuaishou.com/u/fixture".into(),
        "Fixture".into(),
    );
    record.platform_key = Some("kuaishou".into());
    record.monitor_status = false;
    state.store.insert(vec![record]).await.unwrap();
    let expected = state.store.get("paused-page").await.unwrap();
    state
        .scheduler
        .accept_verified(
            &expected,
            &StreamInfo {
                is_live: true,
                anchor_name: "Fixture".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let actual = state.store.get("paused-page").await.unwrap();
    assert!(actual.is_live);
    assert!(!actual.monitor_status && !actual.is_recording);
    assert!(state.engine.active_ids().await.is_empty());
    api::shutdown(&state).await.unwrap();
}
