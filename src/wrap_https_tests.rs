use super::*;
use axum::routing::post;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn optimizer() -> (u16, tokio::task::JoinHandle<()>, Arc<tokio::sync::Notify>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let gate = Arc::new(tokio::sync::Notify::new());
    let response_gate = gate.clone();
    let app = axum::Router::new().route(
        "/v1/messages",
        post(move |request: axum::extract::Request| {
            let gate = response_gate.clone();
            async move {
                assert_eq!(request.uri().query(), Some("beta=true"));
                assert_eq!(request.headers()["authorization"], "Bearer synthetic-login");
                assert_eq!(request.headers()["anthropic-beta"], "fixture");
                assert!(!request.headers().contains_key("proxy-authorization"));
                assert!(!request.headers().contains_key("x-hop"));
                assert_eq!(
                    axum::body::to_bytes(request.into_body(), 2_000_000)
                        .await
                        .unwrap(),
                    "{\"stream\":true}"
                );
                let (sender, chunks) = tokio::sync::mpsc::channel(2);
                sender
                    .try_send(Ok::<_, std::io::Error>("data: one\n\n"))
                    .unwrap();
                tokio::spawn(async move {
                    gate.notified().await;
                    let _ = sender.send(Ok("data: two\n\n")).await;
                });
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .header("connection", "x-response-hop")
                    .header("x-response-hop", "remove")
                    .body(Body::from_stream(
                        tokio_stream::wrappers::ReceiverStream::new(chunks),
                    ))
                    .unwrap()
            }
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (port, task, gate)
}

fn client(bridge: &Bridge) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::https(bridge.url()).unwrap())
        .add_root_certificate(
            reqwest::Certificate::from_pem(&std::fs::read(bridge.ca_path()).unwrap()).unwrap(),
        )
        .build()
        .unwrap()
}

#[tokio::test]
async fn native_tls_streams_authenticated_requests_through_one_optimizer_and_cleans_certificates() {
    let (port, optimizer, gate) = optimizer().await;
    for _ in 0..2 {
        let bridge = Bridge::start(port, None).await.unwrap();
        let ca_path = bridge.ca_path().to_owned();
        let mut response = tokio::time::timeout(
            Duration::from_secs(3),
            client(&bridge)
                .post("https://api.anthropic.com/v1/messages?beta=true")
                .header("authorization", "Bearer synthetic-login")
                .header("anthropic-beta", "fixture")
                .header("proxy-authorization", "do-not-forward")
                .header("connection", "x-hop")
                .header("x-hop", "remove")
                .body("{\"stream\":true}")
                .send(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert!(!response.headers().contains_key("x-response-hop"));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), response.chunk())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            "data: one\n\n"
        );
        gate.notify_one();
        assert_eq!(response.text().await.unwrap(), "data: two\n\n");
        drop(bridge);
        assert!(!ca_path.exists());
    }
    optimizer.abort();
}

#[tokio::test]
async fn plaintext_and_oversized_requests_never_reach_the_optimizer() {
    let bridge = Bridge::start(1, None).await.unwrap();
    let direct = reqwest::Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        direct
            .post(format!("{}/v1/messages", bridge.url()))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client(&bridge)
            .post("https://api.anthropic.com/v1/messages")
            .body(vec![b'x'; super::BODY_LIMIT + 1])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    // A malformed peer disconnect must not take down the listener.
    let mut peer = tokio::net::TcpStream::connect(bridge.address)
        .await
        .unwrap();
    peer.write_all(
        b"CONNECT api.anthropic.com:443 HTTP/1.1\r\nHost: api.anthropic.com:443\r\n\r\n",
    )
    .await
    .unwrap();
    drop(peer);
    assert_eq!(
        direct.get(bridge.url()).send().await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn opaque_tunnels_preserve_idle_connections_and_half_close() {
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = echo.local_addr().unwrap();
    let echo_task = tokio::spawn(async move {
        let (mut socket, _) = echo.accept().await.unwrap();
        let mut data = Vec::new();
        socket.read_to_end(&mut data).await.unwrap();
        socket.write_all(&data).await.unwrap();
    });
    let bridge = Bridge::start(1, None).await.unwrap();
    let mut socket = tokio::net::TcpStream::connect(bridge.address)
        .await
        .unwrap();
    socket
        .write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        header.push(socket.read_u8().await.unwrap());
        assert!(header.len() < 4096);
    }
    assert!(header.starts_with(b"HTTP/1.1 200"));
    tokio::time::sleep(Duration::from_millis(80)).await;
    socket.write_all(b"opaque TLS bytes").await.unwrap();
    socket.shutdown().await.unwrap();
    let mut echoed = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut echoed))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(echoed, b"opaque TLS bytes");
    echo_task.await.unwrap();
}

#[tokio::test]
async fn shared_proxy_health_requires_lean_ctx_identity() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().route(
        "/health",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({"service":"another-service"}))
        }),
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    assert!(
        proxy_ready(&upstream_client().unwrap(), port)
            .await
            .unwrap_err()
            .contains("occupied")
    );
    task.abort();
}
