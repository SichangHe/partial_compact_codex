use crate::storage::{Error, Result};
use std::fs;
use std::io;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub struct ProxyConfig {
    pub listen: String,
    pub upstream: String,
    pub codex_bin: String,
    pub launch_upstream: bool,
}

pub fn serve(config: ProxyConfig) -> Result<()> {
    let listen = Endpoint::parse(&config.listen)?;
    let upstream = Endpoint::parse(&config.upstream)?;
    prepare_listen_endpoint(&listen)?;
    prepare_upstream_endpoint(&upstream, config.launch_upstream)?;
    let mut upstream_process = if config.launch_upstream {
        Some(Upstream::start(&config.codex_bin, &config.upstream)?)
    } else {
        None
    };
    if let Some(process) = upstream_process.as_mut() {
        process.wait_until_ready(&upstream)?;
    }
    println!("pcodx_proxy_listen={}", config.listen);
    println!("upstream_codex_app_server={}", config.upstream);
    println!("codex_frontend=codex --remote {}", config.listen);
    println!("pcodx_live_context_mutation=none");
    println!("native_codex_mutations=relayed");
    println!(
        "partial_compaction_blocker=Codex app-server 0.142.3 exposes no documented API for replacing an arbitrary prior turn range in place while preserving the native KV cache"
    );
    match (listen, upstream) {
        (Endpoint::Unix(listen), Endpoint::Unix(upstream)) => serve_unix(&listen, &upstream),
        (Endpoint::Ws(listen), Endpoint::Ws(upstream)) => serve_ws(&listen, &upstream),
        _ => Err(Error::Invalid(
            "listen and upstream must use the same transport".to_owned(),
        )),
    }
}

fn serve_unix(listen: &Path, upstream: &Path) -> Result<()> {
    let listener = UnixListener::bind(listen)?;
    for client in listener.incoming() {
        relay_unix(client?, UnixStream::connect(upstream)?)?;
    }
    Ok(())
}

fn serve_ws(listen: &str, upstream: &str) -> Result<()> {
    let listener = TcpListener::bind(listen)?;
    for client in listener.incoming() {
        relay_tcp(client?, TcpStream::connect(upstream)?)?;
    }
    Ok(())
}

fn relay_unix(client: UnixStream, upstream: UnixStream) -> Result<()> {
    let client_read = client.try_clone()?;
    let upstream_read = upstream.try_clone()?;
    let up = thread::spawn(move || copy_unix_and_shutdown(client_read, upstream));
    let down = thread::spawn(move || copy_unix_and_shutdown(upstream_read, client));
    join_copy(up)?;
    join_copy(down)?;
    Ok(())
}

fn relay_tcp(client: TcpStream, upstream: TcpStream) -> Result<()> {
    let client_read = client.try_clone()?;
    let upstream_read = upstream.try_clone()?;
    let up = thread::spawn(move || copy_tcp_and_shutdown(client_read, upstream));
    let down = thread::spawn(move || copy_tcp_and_shutdown(upstream_read, client));
    join_copy(up)?;
    join_copy(down)?;
    Ok(())
}

fn copy_unix_and_shutdown(mut read: UnixStream, mut write: UnixStream) -> io::Result<u64> {
    let n_bytes = io::copy(&mut read, &mut write)?;
    write.shutdown(std::net::Shutdown::Write)?;
    Ok(n_bytes)
}

fn copy_tcp_and_shutdown(mut read: TcpStream, mut write: TcpStream) -> io::Result<u64> {
    let n_bytes = io::copy(&mut read, &mut write)?;
    write.shutdown(Shutdown::Write)?;
    Ok(n_bytes)
}

fn join_copy(handle: thread::JoinHandle<io::Result<u64>>) -> Result<()> {
    handle
        .join()
        .map_err(|_| Error::Invalid("proxy copy thread panicked".to_owned()))??;
    Ok(())
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn prepare_listen_endpoint(endpoint: &Endpoint) -> Result<()> {
    match endpoint {
        Endpoint::Unix(path) => {
            ensure_parent(path)?;
            remove_dead_socket(path)
        }
        Endpoint::Ws(_) => Ok(()),
    }
}

fn prepare_upstream_endpoint(endpoint: &Endpoint, launch_upstream: bool) -> Result<()> {
    if !launch_upstream {
        return Ok(());
    }
    match endpoint {
        Endpoint::Unix(path) => {
            ensure_parent(path)?;
            remove_dead_socket(path)
        }
        Endpoint::Ws(_) => Ok(()),
    }
}

fn remove_dead_socket(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_socket() {
        return Err(Error::Invalid(format!(
            "refusing to remove non-socket path {}",
            path.display()
        )));
    }
    if UnixStream::connect(path).is_ok() {
        return Err(Error::Invalid(format!(
            "refusing to remove active socket {}",
            path.display()
        )));
    }
    fs::remove_file(path)?;
    Ok(())
}

enum Endpoint {
    Unix(PathBuf),
    Ws(String),
}

impl Endpoint {
    fn parse(value: &str) -> Result<Self> {
        if let Some(path) = value.strip_prefix("unix://") {
            if path.is_empty() {
                return Err(Error::Invalid("unix endpoint needs a path".to_owned()));
            }
            return Ok(Self::Unix(PathBuf::from(path)));
        }
        if let Some(addr) = value.strip_prefix("ws://") {
            if addr.is_empty() || addr.contains('/') {
                return Err(Error::Invalid(
                    "ws endpoint must be ws://HOST:PORT".to_owned(),
                ));
            }
            return Ok(Self::Ws(addr.to_owned()));
        }
        Err(Error::Invalid(
            "endpoint must start with ws:// or unix://".to_owned(),
        ))
    }
}

struct Upstream {
    child: Child,
    started_at: Instant,
}

impl Upstream {
    fn start(codex_bin: &str, upstream: &str) -> Result<Self> {
        let child = Command::new(codex_bin)
            .args(["app-server", "--listen", upstream])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;
        Ok(Self {
            child,
            started_at: Instant::now(),
        })
    }

    fn wait_until_ready(&mut self, upstream: &Endpoint) -> Result<()> {
        while self.started_at.elapsed() < Duration::from_secs(10) {
            if endpoint_is_ready(upstream) {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait()? {
                return Err(Error::Invalid(format!(
                    "Codex app-server exited before accepting {upstream}: {status}"
                )));
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err(Error::Invalid(format!(
            "timed out waiting for Codex app-server endpoint {upstream}"
        )))
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn endpoint_is_ready(endpoint: &Endpoint) -> bool {
    match endpoint {
        Endpoint::Unix(path) => UnixStream::connect(path).is_ok(),
        Endpoint::Ws(addr) => TcpStream::connect(addr).is_ok(),
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unix(path) => write!(f, "unix://{}", path.display()),
            Self::Ws(addr) => write!(f, "ws://{addr}"),
        }
    }
}
