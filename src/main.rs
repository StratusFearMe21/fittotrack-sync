use std::fs::Permissions;
use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Bytes;
use axum::extract::{connect_info, BodyStream, Query};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{BoxError, Router};
use futures::{ready, Stream, TryStreamExt};
use hyper::client::connect::{Connected, Connection};
use hyper::server::accept::Accept;
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::unix::UCred;
use tokio::net::{UnixListener, UnixStream};
use tokio_util::io::StreamReader;
use url::Url;

static CONFIG: OnceCell<Config> = OnceCell::new();

#[derive(Deserialize, Serialize, Debug)]
struct Server {
    bind: Url,
    unix_permissions: Option<String>,
    command: Option<String>,
    gpx_backup_location: PathBuf,
    ftb_backup_location: PathBuf,
    key: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct Config {
    server: Server,
}

#[derive(Deserialize)]
struct KeyQuery {
    k: String,
}

async fn ftb_handler(
    Query(key): Query<KeyQuery>,
    stream: BodyStream,
) -> Result<String, (StatusCode, String)> {
    let config = unsafe { CONFIG.get_unchecked() };
    if config.server.key == key.k {
        stream_to_file(config.server.ftb_backup_location.join("backup.ftb"), stream).await?;
        if let Some(command) = config.server.command.as_ref() {
            let mut command_iter = command.split_whitespace();
            Command::new(command_iter.next().unwrap())
                .args(command_iter)
                .status()
                .unwrap();
        }
        Ok("Success".to_string())
    } else {
        Err((StatusCode::UNAUTHORIZED, "Invalid Key".to_string()))
    }
}

async fn gpx_handler(
    Query(key): Query<KeyQuery>,
    stream: BodyStream,
) -> Result<String, (StatusCode, String)> {
    let config = unsafe { CONFIG.get_unchecked() };
    if config.server.key == key.k {
        stream_to_file(
            config.server.gpx_backup_location.join(&format!(
                "{}.gpx",
                std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
            )),
            stream,
        )
        .await?;
        if let Some(command) = config.server.command.as_ref() {
            let mut command_iter = command.split_whitespace();
            Command::new(command_iter.next().unwrap())
                .args(command_iter)
                .status()
                .unwrap();
        }
        Ok("Success".to_string())
    } else {
        Err((StatusCode::UNAUTHORIZED, "Invalid Key".to_string()))
    }
}

struct ServerAccept {
    uds: UnixListener,
}

impl Accept for ServerAccept {
    type Conn = UnixStream;
    type Error = BoxError;

    fn poll_accept(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        let (stream, _addr) = ready!(self.uds.poll_accept(cx))?;
        Poll::Ready(Some(Ok(stream)))
    }
}

struct ClientConnection {
    stream: UnixStream,
}

impl AsyncWrite for ClientConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

impl AsyncRead for ClientConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl Connection for ClientConnection {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct UdsConnectInfo {
    peer_addr: Arc<tokio::net::unix::SocketAddr>,
    peer_cred: UCred,
}

impl connect_info::Connected<&UnixStream> for UdsConnectInfo {
    fn connect_info(target: &UnixStream) -> Self {
        let peer_addr = target.peer_addr().unwrap();
        let peer_cred = target.peer_cred().unwrap();

        Self {
            peer_addr: Arc::new(peer_addr),
            peer_cred,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    if let Some(opt) = std::env::args().nth(1) {
        if opt == "setup" {
            let bind: Url = dialoguer::Input::new()
                .with_prompt("Bind address")
                .default("http://127.0.0.1:8080".parse().unwrap())
                .interact_text()
                .unwrap();
            let unix_permissions = if bind.scheme() == "unix" {
                Some(
                    dialoguer::Input::new()
                        .with_prompt("Unix Socket Permissions")
                        .default("770".to_string())
                        .interact_text()
                        .unwrap(),
                )
            } else {
                None
            };
            let gpx_backup_location: PathBuf = Path::new(
                &dialoguer::Input::<String>::new()
                    .with_prompt("GPX Backup Location")
                    .default("./fittotrack/gpx".to_string())
                    .interact_text()
                    .unwrap(),
            )
            .to_path_buf();
            let ftb_backup_location: PathBuf = Path::new(
                &dialoguer::Input::<String>::new()
                    .with_prompt("ftb Backup Location")
                    .default(
                        Path::new(&gpx_backup_location)
                            .parent()
                            .unwrap()
                            .join("ftb")
                            .display()
                            .to_string(),
                    )
                    .interact_text()
                    .unwrap(),
            )
            .to_path_buf();
            let key = nanoid::nanoid!();
            let config = Config {
                server: Server {
                    bind,
                    unix_permissions,
                    command: None,
                    gpx_backup_location,
                    ftb_backup_location,
                    key,
                },
            };
            std::fs::write("./config.toml", toml::to_vec(&config).unwrap())?;
            let server_url: Url = dialoguer::Input::new()
                .with_prompt("Server Domain Name/IP Address")
                .default("https://example.com".parse().unwrap())
                .interact_text()
                .unwrap();
            let gpx_url = format!("{}gpx?k={}", server_url, config.server.key);
            let _ = Command::new("qrencode")
                .args(&["-t", "UTF8", &gpx_url])
                .status();
            println!("GPX Backup URL: {}", gpx_url);
            let ftb_url = format!("{}ftb?k={}", server_url, config.server.key);
            let _ = Command::new("qrencode")
                .args(&["-t", "UTF8", &ftb_url])
                .status();
            println!("FTB Backup URL: {}", ftb_url);
            return Ok(());
        }
    }
    let config = unsafe {
        let map = memmap::Mmap::map(&std::fs::File::open("./config.toml")?)?;
        CONFIG
            .try_insert(toml::from_slice(map.as_ref()).unwrap())
            .unwrap()
    };
    let app = Router::new()
        .route("/ftb", post(ftb_handler))
        .route("/gpx", post(gpx_handler));
    match config.server.bind.scheme() {
        "unix" => {
            let path = Path::new(config.server.bind.path());

            let _ = tokio::fs::remove_file(&path).await;
            tokio::fs::create_dir_all(path.parent().unwrap()).await?;

            let uds = UnixListener::bind(&path).unwrap();
            tokio::fs::set_permissions(
                &path,
                Permissions::from_mode(
                    u32::from_str_radix(
                        config.server.unix_permissions.as_deref().unwrap_or("770"),
                        8,
                    )
                    .unwrap(),
                ),
            )
            .await?;

            axum::Server::builder(ServerAccept { uds })
                .serve(app.into_make_service_with_connect_info::<UdsConnectInfo>())
                .await
                .unwrap();
        }
        "http" => {
            let ip: IpAddr = config.server.bind.host_str().unwrap().parse().unwrap();
            axum::Server::bind(&(ip, config.server.bind.port_or_known_default().unwrap()).into())
                .serve(app.into_make_service())
                .await
                .unwrap()
        }
        "https" => eprintln!(
            "This program is not compatible with HTTPS, \
                             please use this behind a reverse proxy and use \
                             the unix protocol instead"
        ),
        _ => eprintln!(
            "This program is only compatible with \
                       http and unix protocols"
        ),
    }
    Ok(())
}

// Save a `Stream` to a file
async fn stream_to_file<S, E>(path: PathBuf, stream: S) -> Result<String, (StatusCode, String)>
where
    S: Stream<Item = Result<Bytes, E>>,
    E: Into<BoxError>,
{
    async {
        // Convert the stream into an `AsyncRead`.
        let body_with_io_error =
            stream.map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err));
        let body_reader = StreamReader::new(body_with_io_error);
        futures::pin_mut!(body_reader);

        let mut file = tokio::io::BufWriter::new(tokio::fs::File::create(path).await?);

        // Copy the body into the file.
        tokio::io::copy(&mut body_reader, &mut file).await?;

        Ok::<_, std::io::Error>("Success".to_string())
    }
    .await
    .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
}
