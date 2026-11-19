use anyhow::{Context, Result, ensure};
use std::{
    io,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    thread,
};
use tokio::{
    io::copy_bidirectional,
    net::TcpStream,
    sync::{oneshot, watch},
    task::JoinSet,
};

#[derive(Clone, Copy)]
pub struct Route {
    pub http: SocketAddr,
    pub socks: SocketAddr,
}

pub struct Relay {
    route: watch::Sender<Option<Route>>,
    stop: Option<oneshot::Sender<()>>,
    worker: Option<thread::JoinHandle<io::Result<()>>>,
}

async fn serve(
    listener: TcpListener,
    http: bool,
    route: watch::Receiver<Option<Route>>,
) -> io::Result<()> {
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (mut client, _) = accepted?;
                if let Some(selected) = *route.borrow() {
                    let endpoint = if http {selected.http} else {selected.socks};
                    clients.spawn(async move {
                        let mut upstream = TcpStream::connect(endpoint).await?;
                        copy_bidirectional(&mut client, &mut upstream).await?;
                        io::Result::Ok(())
                    });
                }
            }
            Some(result) = clients.join_next(), if !clients.is_empty() => {
                match result {
                    Ok(Ok(()) | Err(_)) => continue,
                    Err(error) => return Err(io::Error::other(error)),
                }
            }
        }
    }
}

impl Relay {
    pub fn open(http: Option<u16>, socks: Option<u16>) -> Result<Self> {
        let mut listeners = Vec::new();
        for (port, is_http) in [(http, true), (socks, false)] {
            if let Some(port) = port {
                let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
                    .with_context(|| format!("Локальный порт {port} недоступен"))?;
                listener.set_nonblocking(true)?;
                listeners.push((listener, is_http));
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (route, changes) = watch::channel(None);
        let (stop, stopped) = oneshot::channel();
        let worker = thread::Builder::new().name("proxywatch-relay".into()).spawn(move || runtime.block_on(async move {
            let mut servers = JoinSet::new();
            for (listener, http) in listeners {
                servers.spawn(serve(listener,http,changes.clone()));
            }
            tokio::select! {
                _ = stopped => Ok(()),
                ended = servers.join_next() => ended.ok_or_else(|| io::Error::other("Нет локальных входов"))?.map_err(io::Error::other)?,
            }
        }))?;
        Ok(Self {
            route,
            stop: Some(stop),
            worker: Some(worker),
        })
    }

    pub fn set(&self, route: Option<Route>) -> Result<()> {
        ensure!(
            !self.worker.as_ref().is_some_and(|w| w.is_finished()),
            "Обработчик локальных входов остановлен"
        );
        self.route.send_replace(route);
        Ok(())
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take()
            && !matches!(worker.join(), Ok(Ok(())))
        {
            eprintln!("Обработчик локальных входов завершился с ошибкой");
        }
    }
}
