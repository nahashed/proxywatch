use crate::runtime_text::message;
use crate::{config::private_config, relay::Route, storage::Settings, subscription::Node};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Proxy, Url, blocking::Client, redirect::Policy};
use std::{
    io::{Seek, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub struct Engine {
    child: Child,
    pub route: Route,
    pub client: Client,
}

impl Engine {
    pub fn start(settings: &Settings, node: &Node) -> Result<Self> {
        let timeout = Duration::from_secs(settings.timeout_seconds);
        let reserve = |_| TcpListener::bind((Ipv4Addr::LOCALHOST, 0));
        let http_listener = settings
            .http_port
            .map(reserve)
            .transpose()
            .context(message!(reserve_http))?;
        let socks_listener = settings
            .socks_port
            .map(reserve)
            .transpose()
            .context(message!(reserve_socks))?;
        let http = http_listener
            .as_ref()
            .map(TcpListener::local_addr)
            .transpose()?;
        let socks = socks_listener
            .as_ref()
            .map(TcpListener::local_addr)
            .transpose()?;
        let config = private_config(
            node,
            http.map(|addr| addr.port()),
            socks.map(|addr| addr.port()),
        )?;
        let proxy = match (http, socks) {
            (Some(addr), _) => format!("http://{addr}"),
            (None, Some(addr)) => format!("socks5h://{addr}"),
            (None, None) => bail!(message!(no_listeners)),
        };
        let mut input = tempfile::tempfile().context(message!(prepare_xray))?;
        serde_json::to_writer(&mut input, &config)?;
        input.flush()?;
        input.rewind()?;
        let client = Client::builder()
            .no_proxy()
            .proxy(Proxy::all(proxy)?)
            .redirect(Policy::none())
            .user_agent("Proxywatch/0.1")
            .build()
            .context(message!(create_probe))?;
        drop(http_listener);
        drop(socks_listener);
        let child = Command::new(&settings.xray_binary)
            .args(["run", "-format", "json", "-config", "stdin:"])
            .env_remove("XRAY_LOCATION_CONFDIR")
            .env_remove("xray.location.confdir")
            .stdin(Stdio::from(input))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context(message!(start_xray))?;
        let mut engine = Self {
            child,
            route: Route { http, socks },
            client,
        };
        let start = Instant::now();
        loop {
            ensure!(engine.child.try_wait()?.is_none(), message!(xray_exited));
            if [http, socks].iter().flatten().all(|address| {
                TcpStream::connect_timeout(address, Duration::from_millis(50)).is_ok()
            }) {
                return Ok(engine);
            }
            ensure!(start.elapsed() < timeout, message!(xray_timeout));
            thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn probe(&self, address: &str, timeout: Duration) -> Result<()> {
        let url = Url::parse(address).map_err(|_| anyhow::anyhow!(message!(invalid_probe_url)))?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none(),
            message!(probe_url_credentials)
        );
        let response = self
            .client
            .get(url)
            .timeout(timeout)
            .send()
            .map_err(|error| {
                if error.is_timeout() {
                    anyhow::anyhow!(message!(probe_timeout))
                } else {
                    anyhow::anyhow!(message!(probe_failed))
                }
            })?;
        let status = response.status();
        if status.is_success() || status.is_redirection() {
            Ok(())
        } else {
            bail!(message!(probe_status, status.as_u16()))
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if self.child.kill().is_err() && !matches!(self.child.try_wait(), Ok(Some(_))) {
            eprintln!("{}", message!(stop_xray));
        }
        if self.child.wait().is_err() {
            eprintln!("{}", message!(wait_xray));
        }
    }
}
