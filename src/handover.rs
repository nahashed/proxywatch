use crate::{
    engine::Engine,
    runtime_text::message,
    storage::{Paths, Settings},
    subscription,
    user_service::UserService,
    xray_listeners::{self, XrayListeners},
};
use anyhow::{Result, anyhow, ensure};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use reqwest::{Proxy, blocking::Client, redirect::Policy};
use std::{
    env, fs,
    io::{self, Write},
    net::{Ipv4Addr, TcpListener},
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        OnceLock,
        atomic::{AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub enum StartMode {
    UserService,
    Foreground,
}

pub fn available(paths: &Paths, explicit_directory: bool) -> bool {
    cfg!(target_os = "linux")
        && !explicit_directory
        && fs::metadata("/proc/self").is_ok_and(|p| p.uid() == 0)
        && env::current_exe().is_ok_and(|p| p == Path::new("/usr/local/bin/proxywatch"))
        && Paths::resolve(None).is_ok_and(|default| default.directory() == paths.directory())
        && paths.lock().is_ok()
}

pub fn conflicts(settings: &Settings) -> Vec<u16> {
    [settings.http_port, settings.socks_port]
        .into_iter()
        .flatten()
        .filter(|port| TcpListener::bind((Ipv4Addr::LOCALHOST, *port)).is_err())
        .collect()
}

// The TUI and the monitor are separate processes. Defer terminal signals during the
// transfer so an interrupt cannot leave the original service stopped halfway through.
static SIGNAL_STATE: AtomicU8 = AtomicU8::new(0);
struct InterruptGuard;
impl InterruptGuard {
    fn begin() -> Result<Self> {
        static INSTALLED: OnceLock<bool> = OnceLock::new();
        let installed = INSTALLED.get_or_init(|| {
            ctrlc::set_handler(|| {
                if SIGNAL_STATE
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |state| {
                        (state != 0).then_some(2)
                    })
                    .is_err()
                {
                    std::process::exit(130);
                }
            })
            .is_ok()
        });
        ensure!(*installed, message!(install_signal_handler));
        SIGNAL_STATE.store(1, Ordering::SeqCst);
        Ok(Self)
    }

    fn interrupted(&self) -> bool {
        SIGNAL_STATE.load(Ordering::SeqCst) == 2
    }
}

impl Drop for InterruptGuard {
    fn drop(&mut self) {
        SIGNAL_STATE.store(0, Ordering::SeqCst);
    }
}

pub fn verify_route(settings: &Settings) -> Result<()> {
    let interrupt = InterruptGuard::begin()?;
    let subscription = subscription::fetch(&settings.subscription_url, &settings.subscription_hwid)
        .map_err(|_| anyhow!(message!(handover_probe)))?;
    for node in &subscription.nodes {
        ensure!(!interrupt.interrupted(), message!(handover_probe));
        let Ok(engine) = Engine::start(settings, node) else {
            continue;
        };
        let ports = (
            engine.route.http.map(|a| a.port()),
            engine.route.socks.map(|a| a.port()),
        );
        // The engine's readiness connections may still be closing while /proc is enumerated.
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if interrupt.interrupted() {
                return Err(anyhow!(message!(handover_probe)));
            }
            if xray_listeners::owns_listeners(engine.pid(), ports.0, ports.1) {
                if engine
                    .probe(
                        &settings.check_url,
                        Duration::from_secs(settings.timeout_seconds),
                    )
                    .is_ok()
                    && !interrupt.interrupted()
                {
                    return Ok(());
                }
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    Err(anyhow!(message!(handover_probe)))
}

fn system_xray(action: &str) -> bool {
    Command::new("/usr/bin/systemctl")
        .args([
            "--system",
            "--no-ask-password",
            "--no-pager",
            action,
            "xray.service",
        ])
        .env_remove("DBUS_SYSTEM_BUS_ADDRESS")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .status()
        .is_ok_and(|status| status.success())
}

enum Target {
    Service(UserService<'static>),
    Terminal(Option<Child>),
}

impl Target {
    fn prepare(paths: &Paths, mode: StartMode) -> Result<Self> {
        match mode {
            StartMode::Foreground => Ok(Self::Terminal(None)),
            StartMode::UserService => UserService::installed(paths.directory(), false)
                .filter(|service| service.stopped())
                .map(Self::Service)
                .ok_or_else(|| anyhow!(message!(handover_changed))),
        }
    }

    fn start(&mut self) -> bool {
        match self {
            Self::Service(service) => service.start(),
            Self::Terminal(slot) => {
                *slot = Command::new("/usr/local/bin/proxywatch")
                    .arg("run")
                    .stdin(Stdio::null())
                    .process_group(0)
                    .spawn()
                    .ok();
                slot.is_some()
            }
        }
    }

    fn pid(&mut self) -> Option<u32> {
        match self {
            Self::Service(service) => service.pid(),
            Self::Terminal(slot) => {
                let child = slot.as_mut()?;
                child.try_wait().ok()?.is_none().then(|| child.id())
            }
        }
    }

    fn stop(&mut self) -> bool {
        match self {
            Self::Service(service) => service.stop(),
            Self::Terminal(None) => true,
            Self::Terminal(Some(child)) => {
                if child.try_wait().is_ok_and(|status| status.is_some()) {
                    return true;
                }
                // The child and its Xray instances share their own process group.
                let group = Pid::from_raw(child.id() as i32);
                let _ = killpg(group, Signal::SIGTERM);
                let deadline = Instant::now() + Duration::from_secs(8);
                while Instant::now() < deadline {
                    if child.try_wait().is_ok_and(|status| status.is_some()) {
                        return true;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                killpg(group, Signal::SIGKILL).is_ok() && child.wait().is_ok()
            }
        }
    }
}

fn wait_for_route(
    target: &mut Target,
    settings: &Settings,
    interrupt: &InterruptGuard,
) -> Result<()> {
    let mut clients = Vec::new();
    for (scheme, port) in [
        ("http", settings.http_port),
        ("socks5h", settings.socks_port),
    ] {
        if let Some(port) = port {
            clients.push(
                Client::builder()
                    .no_proxy()
                    .proxy(Proxy::all(format!("{scheme}://127.0.0.1:{port}"))?)
                    .redirect(Policy::none())
                    .timeout(Duration::from_secs(settings.timeout_seconds.min(3)))
                    .build()?,
            );
        }
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    while !interrupt.interrupted() && Instant::now() < deadline {
        let Some(pid) = target.pid() else {
            break;
        };
        if xray_listeners::owns_listeners(pid, settings.http_port, settings.socks_port)
            && clients.iter().all(|client| {
                client
                    .get(&settings.check_url)
                    .send()
                    .is_ok_and(|r| r.status().is_success() || r.status().is_redirection())
            })
            && !interrupt.interrupted()
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(anyhow!(message!(handover_start)))
}

fn restore(settings: &Settings) -> bool {
    if !system_xray("start") {
        return false;
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if xray_listeners::inspect(settings.http_port, settings.socks_port).is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

pub fn cutover(
    paths: &Paths,
    settings: &Settings,
    inspected: &XrayListeners,
    mode: StartMode,
    previous: Option<&Settings>,
) -> Result<()> {
    ensure!(
        available(paths, false) && !inspected.busy,
        message!(handover_changed)
    );
    let interrupt = InterruptGuard::begin()?;
    let mut target = Target::prepare(paths, mode)?;
    paths.with_handover_settings(previous, settings, || {
        ensure!(
            !interrupt.interrupted()
                && (settings.http_port, settings.socks_port) == (inspected.http, inspected.socks)
                && xray_listeners::inspect(inspected.http, inspected.socks).as_ref()
                    == Some(inspected),
            message!(handover_changed)
        );
        // Protect the rollback as well as startup from concurrent configuration writes.
        let mut new_attempted = false;
        let attempt = (|| {
            ensure!(system_xray("stop"), message!(handover_stop));
            ensure!(
                !interrupt.interrupted() && conflicts(settings).is_empty(),
                message!(handover_changed)
            );
            new_attempted = true;
            ensure!(target.start(), message!(handover_start));
            wait_for_route(&mut target, settings, &interrupt)
        })();
        if attempt.is_err() {
            let stopped = !new_attempted || target.stop();
            let restored = restore(settings);
            ensure!(stopped, message!(handover_target_stop));
            ensure!(restored, message!(handover_restore_failed));
            return Err(anyhow!(message!(handover_restored)));
        }
        Ok(())
    })?;
    if matches!(mode, StartMode::Foreground) {
        let _ = writeln!(io::stderr().lock(), "{}", message!(handover_foreground));
        while !interrupt.interrupted() && target.pid().is_some() {
            thread::sleep(Duration::from_millis(150));
        }
        let stopped = target.stop();
        let restored = restore(settings);
        ensure!(stopped, message!(handover_target_stop));
        ensure!(restored, message!(handover_restore_failed));
        let _ = writeln!(io::stderr().lock(), "{}", message!(handover_returned));
    }
    Ok(())
}
