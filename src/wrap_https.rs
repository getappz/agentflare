//! Experimental HTTP/1.1 bridge; shortcut: 2 MB requests, no WebSocket/Remote Control support.
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, HeaderName, Method, Request, Response, StatusCode};
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, rustls};

const HOST: &str = "api.anthropic.com";
pub(super) const PROXY_PORT: u16 = 4444;
const BODY_LIMIT: usize = 2_000_000;

pub(super) struct Bridge {
    directory: tempfile::TempDir,
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

struct Relay {
    tls: TlsAcceptor,
    client: reqwest::Client,
    lean_port: u16,
}

impl Bridge {
    pub(super) async fn start(lean_port: u16, previous_ca: Option<&Path>) -> Result<Self, String> {
        let directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let (tls, mut pem) = certificates()?;
        if let Some(previous) = previous_ca {
            pem.push('\n');
            pem.push_str(
                &std::fs::read_to_string(previous)
                    .map_err(|_| "Cannot read existing NODE_EXTRA_CA_CERTS")?,
            );
        }
        std::fs::write(directory.path().join("ca.pem"), pem).map_err(|error| error.to_string())?;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let relay = Arc::new(Relay {
            tls,
            client: upstream_client()?,
            lean_port,
        });
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let relay = relay.clone();
                tokio::spawn(async move {
                    // Peer disconnects are normal; never log URLs, headers or credentials.
                    let _ = http1::Builder::new()
                        .max_buf_size(32768)
                        .serve_connection(
                            TokioIo::new(stream),
                            service_fn(move |request| frontend(request, relay.clone())),
                        )
                        .with_upgrades()
                        .await;
                });
            }
        });
        Ok(Self {
            directory,
            address,
            task,
        })
    }

    pub(super) fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub(super) fn ca_path(&self) -> PathBuf {
        self.directory.path().join("ca.pem")
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn certificates() -> Result<(TlsAcceptor, String), String> {
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
        KeyPair, KeyUsagePurpose,
    };
    let generate = || -> Result<_, Box<dyn std::error::Error>> {
        let now = SystemTime::now();
        let mut ca = CertificateParams::default();
        ca.distinguished_name
            .push(DnType::CommonName, "Agentflare temporary Claude CA");
        ca.not_before = (now - Duration::from_secs(60)).into();
        ca.not_after = (now + Duration::from_secs(86400)).into();
        ca.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca_key = KeyPair::generate()?;
        let ca_cert = ca.self_signed(&ca_key)?;
        let mut leaf = CertificateParams::new(vec![HOST.to_owned()])?;
        leaf.distinguished_name.push(DnType::CommonName, HOST);
        leaf.not_before = ca.not_before;
        leaf.not_after = ca.not_after;
        leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        leaf.use_authority_key_identifier_extension = true;
        let leaf_key = KeyPair::generate()?;
        let leaf_cert = leaf.signed_by(&leaf_key, &Issuer::new(ca, ca_key))?;
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(leaf_key.serialize_der());
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(vec![leaf_cert.der().clone()], key.into())?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok((TlsAcceptor::from(Arc::new(config)), ca_cert.pem()))
    };
    generate().map_err(|error| format!("Cannot create temporary TLS certificates: {error}"))
}

fn upstream_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())
}

fn reply(status: StatusCode) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("connection", "close")
        .body(Body::empty())
        .unwrap()
}

async fn frontend(
    mut request: Request<Incoming>,
    relay: Arc<Relay>,
) -> Result<Response<Body>, Infallible> {
    if request.method() != Method::CONNECT {
        return Ok(reply(StatusCode::FORBIDDEN));
    }
    let Some(authority) = request.uri().authority() else {
        return Ok(reply(StatusCode::BAD_REQUEST));
    };
    let Some(port) = authority.port_u16().filter(|port| *port != 0) else {
        return Ok(reply(StatusCode::BAD_REQUEST));
    };
    let host = authority.host().trim_matches(['[', ']']);
    if host.is_empty() || authority.as_str().contains('@') {
        return Ok(reply(StatusCode::BAD_REQUEST));
    }
    let intercepted = host.eq_ignore_ascii_case(HOST) && port == 443;
    let upstream = if intercepted {
        None
    } else {
        match tokio::time::timeout(Duration::from_secs(30), TcpStream::connect((host, port))).await
        {
            Ok(Ok(stream)) => Some(stream),
            _ => return Ok(reply(StatusCode::BAD_GATEWAY)),
        }
    };
    let upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        let Ok(upgraded) = upgrade.await else {
            return;
        };
        let mut socket = TokioIo::new(upgraded);
        if let Some(mut upstream) = upstream {
            // No idle deadline: SSE, MCP and other network tools may stay silent for minutes.
            let _ = tokio::io::copy_bidirectional(&mut socket, &mut upstream).await;
        } else if let Ok(tls) = relay.tls.accept(socket).await {
            let _ = http1::Builder::new()
                .keep_alive(false)
                .max_buf_size(32768)
                .serve_connection(
                    TokioIo::new(tls),
                    service_fn(move |request| relay_request(request, relay.clone())),
                )
                .await;
        }
    });
    Ok(Response::new(Body::empty()))
}

fn clean_headers(mut headers: HeaderMap) -> HeaderMap {
    let nominated: Vec<HeaderName> = headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in nominated {
        headers.remove(name);
    }
    for name in [
        "host",
        "connection",
        "keep-alive",
        "proxy-authorization",
        "proxy-authenticate",
        "proxy-connection",
        "transfer-encoding",
        "content-length",
        "trailer",
        "te",
        "upgrade",
    ] {
        headers.remove(name);
    }
    headers
}

async fn relay_request(
    request: Request<Incoming>,
    relay: Arc<Relay>,
) -> Result<Response<Body>, Infallible> {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path_and_query().map_or("/", |path| path.as_str());
    if parts.uri.scheme().is_some()
        || parts.uri.authority().is_some()
        || !path.starts_with('/')
        || path.starts_with("//")
    {
        return Ok(reply(StatusCode::BAD_REQUEST));
    }
    if parts.headers.contains_key("transfer-encoding") {
        return Ok(reply(StatusCode::PAYLOAD_TOO_LARGE));
    }
    if let Some(length) = parts.headers.get("content-length") {
        match length
            .to_str()
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
        {
            Some(length) if length <= BODY_LIMIT as u64 => {}
            Some(_) => return Ok(reply(StatusCode::PAYLOAD_TOO_LARGE)),
            None => return Ok(reply(StatusCode::BAD_REQUEST)),
        }
    }
    let Ok(body) = to_bytes(Body::new(body), BODY_LIMIT).await else {
        return Ok(reply(StatusCode::PAYLOAD_TOO_LARGE));
    };
    let url = if parts.method == Method::POST && parts.uri.path() == "/v1/messages" {
        format!("http://127.0.0.1:{}{path}", relay.lean_port)
    } else {
        format!("https://{HOST}{path}")
    };
    let response = relay
        .client
        .request(parts.method, url)
        .headers(clean_headers(parts.headers))
        .body(body)
        .send()
        .await;
    let Ok(response) = response else {
        return Ok(reply(StatusCode::BAD_GATEWAY));
    };
    let status = response.status();
    let headers = clean_headers(response.headers().clone());
    let mut output = Response::new(Body::from_stream(response.bytes_stream()));
    *output.status_mut() = status;
    *output.headers_mut() = headers;
    output
        .headers_mut()
        .insert("connection", "close".parse().unwrap());
    Ok(output)
}

async fn proxy_ready(client: &reqwest::Client, port: u16) -> Result<bool, String> {
    let Ok(mut response) = client
        .get(format!("http://127.0.0.1:{port}/health"))
        .timeout(Duration::from_secs(1))
        .send()
        .await
    else {
        return Ok(false);
    };
    let occupied = || format!("Port {port} is occupied by another service");
    if !response.status().is_success() {
        return Err(occupied());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| occupied())? {
        if bytes.len() + chunk.len() > 4096 {
            return Err(occupied());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| occupied())?;
    if value.get("service").and_then(|value| value.as_str()) != Some("lean-ctx-proxy") {
        return Err(occupied());
    }
    Ok(true)
}

pub(super) async fn shared_proxy(lean: &Path, port: u16) -> Result<(), String> {
    let client = upstream_client()?;
    if proxy_ready(&client, port).await? {
        return Ok(());
    }
    let mut command = Command::new(lean);
    command
        .args(["proxy", "start", "--detach", &format!("--port={port}")])
        .env("LEAN_CTX_PROXY_MODE", "cache")
        .env("LEAN_CTX_NO_UPDATE_CHECK", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, _) in std::env::vars_os() {
        if key
            .to_str()
            .is_some_and(|key| key.to_ascii_uppercase().ends_with("_PROXY"))
        {
            command.env_remove(key);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    let mut starter = command
        .spawn()
        .map_err(|error| format!("Cannot start lean-ctx proxy: {error}"))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let ready = proxy_ready(&client, port).await?;
        match starter.try_wait().map_err(|error| error.to_string())? {
            Some(status) if !status.success() && !ready => {
                return Err(format!("lean-ctx proxy start failed: {status}"));
            }
            Some(_) if ready => return Ok(()),
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            // Only stop our startup helper; never stop the shared optimizer.
            let _ = starter.kill();
            let _ = starter.wait();
            return Err(
                "Run lean-ctx proxy start --detach; default proxy did not become ready".into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
#[path = "wrap_https_tests.rs"]
mod tests;
