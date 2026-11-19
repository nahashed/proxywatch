use serde::{Deserialize, de::IgnoredAny};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{ErrorKind, Read},
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

const LOOPBACK: &str = "127.0.0.1";
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct XrayListeners {
    pub pid: u32,
    // Final assignments include any requested free port, preserving the protocols of occupied ports.
    pub http: Option<u16>,
    pub socks: Option<u16>,
    pub busy: bool,
    started_us: u64,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    inbounds: Vec<Inbound>,
    #[serde(rename = "outbounds")]
    _outbounds: Option<IgnoredAny>,
    #[serde(rename = "routing")]
    _routing: Option<IgnoredAny>,
    #[serde(rename = "dns")]
    _dns: Option<IgnoredAny>,
    #[serde(rename = "policy")]
    _policy: Option<IgnoredAny>,
    #[serde(rename = "log")]
    _log: Option<IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inbound {
    listen: String,
    port: u16,
    protocol: String,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    settings: Options,
    #[serde(rename = "streamSettings", default)]
    stream: Stream,
    #[serde(default)]
    #[serde(rename = "sniffing")]
    _sniffing: Sniffing,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
struct Options {
    auth: Option<String>,
    udp: bool,
    // No account entries can be represented without failing validation.
    accounts: Vec<IgnoredAny>,
    allow_transparent: bool,
    user_level: u32,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Stream {
    network: Option<String>,
    security: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Sniffing {
    #[serde(rename = "enabled")]
    _enabled: bool,
    #[serde(rename = "destOverride")]
    _dest_override: Vec<String>,
    #[serde(rename = "routeOnly")]
    _route_only: bool,
}

#[derive(Default)]
struct Ports {
    http: Option<u16>,
    socks: Option<u16>,
    tags: HashSet<String>,
}

impl Ports {
    fn add(&mut self, inbound: Inbound) -> Option<()> {
        let options = inbound.settings;
        if inbound.listen != LOOPBACK
            || inbound.port == 0
            || options.udp
            || options.allow_transparent
            || !options.accounts.is_empty()
            || options.user_level != 0
            || !matches!(
                inbound.stream.network.as_deref(),
                None | Some("tcp" | "raw")
            )
            || !matches!(inbound.stream.security.as_deref(), None | Some("none"))
            || inbound.tag.is_some_and(|tag| !self.tags.insert(tag))
            || [self.http, self.socks].contains(&Some(inbound.port))
        {
            return None;
        }
        let slot = match (inbound.protocol.as_str(), options.auth.as_deref()) {
            ("http", None) => &mut self.http,
            ("socks", None | Some("noauth")) => &mut self.socks,
            _ => return None,
        };
        slot.replace(inbound.port).is_none().then_some(())
    }

    fn assign(&self, http: Option<u16>, socks: Option<u16>) -> Option<(Option<u16>, Option<u16>)> {
        let requested = [http, socks];
        let known = [self.http, self.socks];
        if known == [None, None]
            || known
                .into_iter()
                .flatten()
                .any(|p| !requested.contains(&Some(p)))
        {
            return None;
        }
        let mut available = requested
            .into_iter()
            .flatten()
            .filter(|p| !known.contains(&Some(*p)));
        let assignment = (
            self.http.or_else(|| available.next()),
            self.socks.or_else(|| available.next()),
        );
        available.next().is_none().then_some(assignment)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Service {
    pid: u32,
    started_us: u64,
    group: PathBuf,
}

fn output(program: &str, args: &[&str]) -> Option<String> {
    let response = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    response
        .status
        .success()
        .then(|| String::from_utf8(response.stdout).ok())?
}

fn independently_stoppable(fields: &HashMap<&str, &str>) -> bool {
    fields.get("RefuseManualStop") == Some(&"no")
        && [
            "BoundBy",
            "RequiredBy",
            "ConsistsOf",
            "PropagatesStopTo",
            "TriggeredBy",
        ]
        .iter()
        .all(|key| fields.get(key) == Some(&""))
}

fn service() -> Option<Service> {
    let properties = output(
        "/usr/bin/systemctl",
        &[
            "--system",
            "show",
            "--all",
            "xray.service",
            "-p",
            "Id,Names,ActiveState,SubState,MainPID,ExecMainPID,ControlGroup",
            "-p",
            "RefuseManualStop,BoundBy,RequiredBy,ConsistsOf,PropagatesStopTo,TriggeredBy",
        ],
    )?;
    let fields: HashMap<_, _> = properties
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    if !independently_stoppable(&fields)
        || fields.get("Id")? != &"xray.service"
        || fields.get("Names")? != &"xray.service"
        || fields.get("ActiveState")? != &"active"
        || fields.get("SubState")? != &"running"
        || fields.get("MainPID")? != fields.get("ExecMainPID")?
    {
        return None;
    }
    let pid: u32 = fields.get("MainPID")?.parse().ok()?;
    let group = Path::new(fields.get("ControlGroup")?)
        .strip_prefix("/")
        .ok()?;
    if pid <= 1
        || group.file_name()? != "xray.service"
        || !group
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let started = output(
        "/usr/bin/busctl",
        &[
            "--system",
            "--timeout=3",
            "get-property",
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1/unit/xray_2eservice",
            "org.freedesktop.systemd1.Service",
            "ExecMainStartTimestamp",
        ],
    )?;
    let started_us = started.trim().strip_prefix("t ")?.parse::<u64>().ok()?;
    (started_us > 0).then(|| Service {
        pid,
        started_us,
        group: Path::new("/sys/fs/cgroup").join(group),
    })
}

fn unchanged(metadata: &fs::Metadata, started_us: u64) -> bool {
    let start = i128::from(started_us) * 1000;
    [
        (metadata.ctime(), metadata.ctime_nsec()),
        (metadata.mtime(), metadata.mtime_nsec()),
    ]
    .iter()
    .all(|&(sec, nsec)| i128::from(sec) * 1_000_000_000 + i128::from(nsec) <= start)
}

fn config_sources(args: &[u8]) -> Option<Vec<(bool, PathBuf)>> {
    let args = args
        .strip_suffix(&[0])?
        .split(|ch| *ch == 0)
        .map(std::str::from_utf8)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let mut flags = args.get(1..)?.iter().copied().peekable();
    if flags.peek() == Some(&"run") {
        flags.next();
    }
    let mut sources = Vec::new();
    while let Some(flag) = flags.next() {
        let flag = flag.strip_prefix("--").or_else(|| flag.strip_prefix('-'))?;
        let (flag, value) = match flag.split_once('=') {
            Some(pair) => pair,
            None => (flag, flags.next()?),
        };
        if flag == "format" && value == "json" {
            continue;
        }
        let directory = match flag {
            "config" | "c" => false,
            "confdir" | "d" => true,
            _ => return None,
        };
        let path = Path::new(value);
        if !path.is_absolute()
            || !path
                .components()
                .all(|p| matches!(p, Component::RootDir | Component::Normal(_)))
        {
            return None;
        }
        sources.push((directory, path.to_owned()));
    }
    (!sources.is_empty()).then_some(sources)
}

fn configuration(process: &Path, started_us: u64) -> Option<Ports> {
    let env = fs::read(process.join("environ")).ok()?;
    if env.split(|ch| *ch == 0).any(|pair| {
        matches!(
            pair.split(|c| *c == b'=').next(),
            Some(
                b"XRAY_LOCATION_CONFDIR"
                    | b"xray.location.confdir"
                    | b"XRAY_LOCATION_CONFIG"
                    | b"xray.location.config"
            )
        )
    }) {
        return None;
    }
    let args = fs::read(process.join("cmdline")).ok()?;
    let mut files = Vec::new();
    for (directory, path) in config_sources(&args)? {
        let mut resolved = process.join("root");
        for part in path.components() {
            if let Component::Normal(name) = part {
                resolved.push(name);
                if fs::symlink_metadata(&resolved).ok()?.is_symlink() {
                    return None;
                }
            }
        }
        // PrivateTmp and read-only bind mounts need not change the file that supplied the config.
        let host = fs::metadata(&path).ok()?;
        let mounted = fs::metadata(&resolved).ok()?;
        if (host.dev(), host.ino()) != (mounted.dev(), mounted.ino()) {
            return None;
        }
        if directory {
            if !mounted.is_dir() || !unchanged(&mounted, started_us) {
                return None;
            }
            for entry in fs::read_dir(&resolved).ok()? {
                let entry = entry.ok()?;
                files.push((path.join(entry.file_name()), entry.path()));
            }
        } else {
            files.push((path, resolved));
        }
    }
    let mut ports = Ports::default();
    for (path, resolved) in files {
        let metadata = fs::symlink_metadata(&resolved).ok()?;
        let host = fs::symlink_metadata(&path).ok()?;
        let identity = (host.dev(), host.ino());
        if !metadata.is_file()
            || (metadata.dev(), metadata.ino()) != identity
            || path.extension()? != "json"
            || !unchanged(&metadata, started_us)
            || metadata.len() > MAX_CONFIG_BYTES
        {
            return None;
        }
        let file = File::open(resolved).ok()?;
        let observed = file.metadata().ok()?;
        if (observed.dev(), observed.ino()) != identity {
            return None;
        }
        let config: Config = serde_json::from_reader((&file).take(MAX_CONFIG_BYTES + 1)).ok()?;
        if !unchanged(&file.metadata().ok()?, started_us) {
            return None;
        }
        for inbound in config.inbounds {
            ports.add(inbound)?;
        }
    }
    Some(ports)
}

fn socket_inodes(process: &Path) -> Option<HashSet<u64>> {
    let mut result = HashSet::new();
    for entry in fs::read_dir(process.join("fd")).ok()? {
        let path = fs::read_link(entry.ok()?.path()).ok()?;
        if let Some(text) = path.to_str().and_then(|s| s.strip_prefix("socket:[")) {
            result.insert(text.strip_suffix(']')?.parse().ok()?);
        }
    }
    Some(result)
}

#[derive(Clone, Copy)]
struct Socket {
    inode: u64,
    port: u16,
    local: bool,
    listen: bool,
    udp: bool,
    active: bool,
}

fn sockets() -> Option<Vec<Socket>> {
    let mut result = Vec::new();
    for table in ["tcp", "tcp6", "udp", "udp6"] {
        let contents = match fs::read_to_string(Path::new("/proc/net").join(table)) {
            Ok(contents) => contents,
            Err(error)
                if table.ends_with('6')
                    && error.kind() == ErrorKind::NotFound
                    && !Path::new("/proc/net/if_inet6").try_exists().ok()? =>
            {
                continue;
            }
            Err(_) => return None,
        };
        for line in contents.lines().skip(1) {
            let mut fields = line.split_ascii_whitespace();
            let (address, port) = fields.nth(1)?.split_once(':')?;
            let state = fields.nth(1)?;
            let inode = fields.nth(5)?.parse().ok()?;
            let udp = table.starts_with("udp");
            let local = table == "tcp"
                && u32::from_str_radix(address, 16).ok()?.to_ne_bytes() == [127, 0, 0, 1];
            result.push(Socket {
                inode,
                port: u16::from_str_radix(port, 16).ok()?,
                local,
                active: !matches!(state, "06" | "07" | "0A"),
                listen: !udp && state == "0A",
                udp,
            });
        }
    }
    Some(result)
}

fn check_sockets(
    owned: &HashSet<u64>,
    rows: &[Socket],
    declared: &Ports,
    requested: [Option<u16>; 2],
) -> Option<bool> {
    let mut found = HashSet::new();
    let mut busy = false;
    for row in rows {
        let ours = owned.contains(&row.inode);
        let wanted = requested.contains(&Some(row.port));
        if row.udp && (ours || wanted) {
            return None;
        }
        if row.listen && (ours || wanted) {
            if !ours
                || !wanted
                || !row.local
                || ![declared.http, declared.socks].contains(&Some(row.port))
                || !found.insert(row.port)
            {
                return None;
            }
        } else if row.active && !row.udp && (ours || wanted) {
            // Includes connections queued for accept even before they have an owned FD.
            busy = true;
        }
    }
    [declared.http, declared.socks]
        .into_iter()
        .flatten()
        .all(|port| found.contains(&port))
        .then_some(busy)
}

fn same_namespace(process: &Path) -> Option<()> {
    let ours = fs::metadata("/proc/self/ns/net").ok()?;
    let theirs = fs::metadata(process.join("ns/net")).ok()?;
    ((ours.dev(), ours.ino()) == (theirs.dev(), theirs.ino())).then_some(())
}

fn isolated_group(service: &Service) -> Option<()> {
    let members = fs::read_to_string(service.group.join("cgroup.procs")).ok()?;
    if members.trim() != service.pid.to_string() {
        return None;
    }
    for entry in fs::read_dir(&service.group).ok()? {
        if entry.ok()?.file_type().ok()?.is_dir() {
            return None;
        }
    }
    Some(())
}

fn other_listeners(owned: &HashSet<u64>) -> Option<bool> {
    let contents = fs::read_to_string("/proc/net/unix").ok()?;
    for row in contents.lines().skip(1) {
        let mut fields = row.split_ascii_whitespace();
        let flags = u32::from_str_radix(fields.nth(3)?, 16).ok()?;
        let inode = fields.nth(2)?.parse().ok()?;
        if flags & 0x10000 != 0 && owned.contains(&inode) {
            return Some(true);
        }
    }
    Some(false)
}

fn exclusively_held(pid: u32, listeners: &HashSet<u64>) -> Option<()> {
    for entry in fs::read_dir("/proc").ok()? {
        let entry = entry.ok()?;
        let Some(other) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if other == pid {
            continue;
        }
        let descriptors = match fs::read_dir(entry.path().join("fd")) {
            Ok(files) => files,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        for descriptor in descriptors {
            match fs::metadata(descriptor.ok()?.path()) {
                Ok(metadata)
                    if metadata.file_type().is_socket() && listeners.contains(&metadata.ino()) =>
                {
                    return None;
                }
                Ok(_) => (),
                Err(error) if error.kind() == ErrorKind::NotFound => (),
                Err(_) => return None,
            }
        }
    }
    Some(())
}

fn valid_request(http: Option<u16>, socks: Option<u16>) -> bool {
    http != socks && http != Some(0) && socks != Some(0)
}

pub(crate) fn inspect(http: Option<u16>, socks: Option<u16>) -> Option<XrayListeners> {
    if !cfg!(target_os = "linux") || !valid_request(http, socks) {
        return None;
    }
    let first = service()?;
    let process = Path::new("/proc").join(first.pid.to_string());
    if fs::read_link(process.join("exe")).ok()?.file_name()? != "xray" {
        return None;
    }
    same_namespace(&process)?;
    isolated_group(&first)?;
    let config = configuration(&process, first.started_us)?;
    let owned = socket_inodes(&process)?;
    if other_listeners(&owned)? {
        return None;
    }
    let rows = sockets()?;
    let busy = check_sockets(&owned, &rows, &config, [http, socks])?;
    let listeners = rows
        .iter()
        .filter(|s| s.listen && owned.contains(&s.inode))
        .map(|s| s.inode)
        .collect();
    exclusively_held(first.pid, &listeners)?;
    let (http, socks) = config.assign(http, socks)?;
    if service()? != first {
        return None;
    }
    Some(XrayListeners {
        pid: first.pid,
        http,
        socks,
        busy,
        started_us: first.started_us,
    })
}

pub(crate) fn owns_listeners(pid: u32, http: Option<u16>, socks: Option<u16>) -> bool {
    fn check(pid: u32, http: Option<u16>, socks: Option<u16>) -> Option<bool> {
        if !cfg!(target_os = "linux") || !valid_request(http, socks) {
            return Some(false);
        }
        let process = Path::new("/proc").join(pid.to_string());
        let owned = socket_inodes(&process)?;
        same_namespace(&process)?;
        let rows = sockets()?;
        Some([http, socks].into_iter().flatten().all(|port| {
            let mut matching = rows.iter().filter(|row| row.listen && row.port == port);
            matching
                .next()
                .is_some_and(|row| row.local && owned.contains(&row.inode))
                && matching.next().is_none()
        }))
    }
    check(pid, http, socks) == Some(true)
}

#[cfg(test)]
#[path = "../tests/unit/xray_listeners.rs"]
mod tests;
