use crate::{
    engine::Engine,
    runtime_text::message,
    storage::{Paths, Settings},
    subscription,
    user_service::{Scope, UserService},
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
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        OnceLock,
        atomic::{AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    Service(Scope),
    Foreground,
}

impl StartMode {
    pub fn detect(paths: &Paths, explicit_directory: bool) -> Self {
        UserService::installed(paths.directory(), explicit_directory)
            .map_or(Self::Foreground, |service| Self::Service(service.scope()))
    }
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
                if SIGNAL_STATE.swap(2, Ordering::SeqCst) == 0 {
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

fn systemctl(args: &[&str]) -> Option<Output> {
    let result = Command::new("/usr/bin/systemctl")
        .args(["--system", "--no-ask-password", "--no-pager"])
        .args(args)
        .env_remove("DBUS_SYSTEM_BUS_ADDRESS")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .output()
        .ok()?;
    result.status.success().then_some(result)
}

fn system_xray(action: &str) -> bool {
    systemctl(&[action, "xray.service"]).is_some()
}

// Only change the standard multi-user target link. Generic `systemctl disable` can
// remove unrelated administrator-created links and propagate to units named in Also=.
#[derive(PartialEq, Eq)]
struct BootLink {
    path: PathBuf,
    fragment: PathBuf,
    previous: Option<PathBuf>,
}

impl BootLink {
    fn read(unit: &str) -> Option<Self> {
        let result = systemctl(&[
            "show",
            "--all",
            "--property=Id,Names,LoadState,NeedDaemonReload,FragmentPath,UnitFileState,WantedBy,RequiredBy,UpheldBy",
            unit,
        ])?;
        let properties = std::str::from_utf8(&result.stdout).ok()?;
        let property = |key| properties.lines().find_map(|line| line.strip_prefix(key));
        if property("Id=")? != unit
            || property("Names=")? != unit
            || property("LoadState=")? != "loaded"
            || property("NeedDaemonReload=")? != "no"
            || !property("RequiredBy=")?.is_empty()
            || !property("UpheldBy=")?.is_empty()
        {
            return None;
        }
        let wants = Path::new("/etc/systemd/system/multi-user.target.wants");
        if !fs::symlink_metadata(wants).ok()?.is_dir() {
            return None;
        }
        let path = wants.join(unit);
        let fragment = PathBuf::from(property("FragmentPath=")?);
        if !fragment.is_absolute() || !fragment.is_file() {
            return None;
        }
        let previous = match fs::read_link(&path) {
            Ok(target)
                if path.canonicalize().ok()? == fragment.canonicalize().ok()?
                    && property("UnitFileState=")? == "enabled"
                    && property("WantedBy=")? == "multi-user.target" =>
            {
                Some(target)
            }
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    && property("UnitFileState=")? == "disabled"
                    && property("WantedBy=")?.is_empty() =>
            {
                None
            }
            _ => return None,
        };
        Some(Self {
            path,
            fragment,
            previous,
        })
    }

    fn matches(&self, expected: Option<&Path>) -> bool {
        match fs::read_link(&self.path) {
            Ok(target) => Some(target.as_path()) == expected,
            Err(e) => e.kind() == io::ErrorKind::NotFound && expected.is_none(),
        }
    }

    fn change(&self, from: Option<&Path>, to: Option<&Path>) -> bool {
        if !self.matches(from) {
            return false;
        }
        if from == to {
            return true;
        }
        match to {
            Some(target) => std::os::unix::fs::symlink(target, &self.path).is_ok(),
            None => fs::remove_file(&self.path).is_ok(),
        }
    }
}

#[derive(PartialEq, Eq)]
pub(crate) struct BootPlan {
    proxywatch: BootLink,
    xray: BootLink,
}

impl BootPlan {
    pub(crate) fn inspect(mode: StartMode) -> Option<Self> {
        if mode != StartMode::Service(Scope::System) {
            return None;
        }
        Some(Self {
            proxywatch: BootLink::read("proxywatch.service")?,
            xray: BootLink::read("xray.service")?,
        })
    }

    fn commit(&self, service: &UserService<'_>, interrupt: &InterruptGuard) -> Result<()> {
        let inspect = || Self::inspect(StartMode::Service(Scope::System));
        ensure!(
            inspect().as_ref() == Some(self) && service.active() && !interrupt.interrupted(),
            message!(handover_changed)
        );
        let proxy = &self.proxywatch;
        let xray = &self.xray;
        let proxy_old = proxy.previous.as_deref();
        let xray_old = xray.previous.as_deref();
        let proxy_new = Some(proxy_old.unwrap_or(&proxy.fragment));
        // Remember exactly which links this transaction changed, including their original spelling.
        ensure!(
            proxy.change(proxy_old, proxy_new),
            message!(handover_boot_failed)
        );
        let xray_changed = xray.change(xray_old, None);
        if xray_changed
            && systemctl(&["daemon-reload"]).is_some()
            && inspect().is_some_and(|plan| {
                plan.proxywatch.fragment == proxy.fragment
                    && plan.xray.fragment == xray.fragment
                    && plan.proxywatch.previous.as_deref() == proxy_new
                    && plan.xray.previous.is_none()
            })
            && service.active()
            && !interrupt.interrupted()
        {
            return Ok(());
        }
        let xray_restored = !xray_changed || xray.change(None, xray_old);
        let proxy_restored = proxy.change(proxy_new, proxy_old);
        ensure!(
            xray_restored
                && proxy_restored
                && systemctl(&["daemon-reload"]).is_some()
                && inspect().as_ref() == Some(self),
            message!(handover_boot_restore)
        );
        Err(anyhow!(message!(handover_boot_failed)))
    }
}

enum Target {
    Service(UserService<'static>),
    Terminal(Option<Child>),
}

impl Target {
    fn prepare(paths: &Paths, mode: StartMode) -> Result<Self> {
        match (mode, UserService::installed(paths.directory(), false)) {
            (StartMode::Foreground, None) => Ok(Self::Terminal(None)),
            (StartMode::Service(expected), Some(service))
                if service.scope() == expected && service.available_for_handover() =>
            {
                Ok(Self::Service(service))
            }
            _ => Err(anyhow!(message!(handover_changed))),
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
    boot: Option<&BootPlan>,
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
        if let Some(boot) = boot {
            ensure!(
                BootPlan::inspect(mode).as_ref() == Some(boot),
                message!(handover_changed)
            );
        }
        if let Target::Service(service) = &target {
            ensure!(service.prepare_handover(), message!(handover_changed));
            ensure!(
                xray_listeners::inspect(inspected.http, inspected.socks).as_ref()
                    == Some(inspected),
                message!(handover_changed)
            );
        }
        // Protect the rollback as well as startup from concurrent configuration writes.
        let mut new_attempted = false;
        let attempt: Result<()> = (|| {
            ensure!(system_xray("stop"), message!(handover_stop));
            ensure!(
                !interrupt.interrupted() && conflicts(settings).is_empty(),
                message!(handover_changed)
            );
            new_attempted = true;
            ensure!(target.start(), message!(handover_start));
            wait_for_route(&mut target, settings, &interrupt)?;
            Ok(())
        })();
        let boot_result = if attempt.is_ok() {
            match (boot, &target) {
                (Some(plan), Target::Service(service)) => plan.commit(service, &interrupt),
                (None, _) => Ok(()),
                _ => Err(anyhow!(message!(handover_changed))),
            }
        } else {
            Ok(())
        };
        if attempt.is_err() || boot_result.is_err() {
            let stopped = !new_attempted || target.stop();
            let restored = restore(settings);
            ensure!(stopped, message!(handover_target_stop));
            ensure!(restored, message!(handover_restore_failed));
            boot_result?;
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
