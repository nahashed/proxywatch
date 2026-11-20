use crate::runtime_text::message;
use serde::{Deserialize, de::IgnoredAny};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{ErrorKind, Read},
    net::{Ipv4Addr, Ipv6Addr},
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

/// Process names come only from verified executable basenames. Never expose command lines.
pub(crate) struct Owner {
    pub pid: u32,
    pub name: Option<String>,
}

pub(crate) struct PortReport {
    pub port: u16,
    pub listening: Option<bool>,
    pub owners: Vec<Owner>,
    pub clients: Option<usize>,
    pub closing: Option<usize>,
}

pub(crate) struct Inspection {
    pub ports: Vec<PortReport>,
    pub listeners: Option<XrayListeners>,
    pub blocker: Option<Blocker>,
    pub other_flows: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blocker {
    InvalidRequest,
    Systemctl,
    Systemd,
    MissingUnit,
    Inactive,
    UnitIdentity,
    StopPolicy,
    Busctl,
    StartTime,
    ExeAccess,
    Executable,
    NamespaceAccess,
    NamespaceMismatch,
    CgroupAccess,
    CgroupShared,
    ProcessConfig,
    ConfigSource,
    ConfigAccess,
    ConfigIdentity,
    ConfigChanged,
    ConfigFormat,
    ConfigFields,
    InboundPolicy,
    AdditionalInbound,
    Descriptors,
    SocketTables,
    UnixListener,
    Udp,
    ForeignListener,
    OtherListener,
    BindAddress,
    SocketMismatch,
    ExclusiveAccess,
    SharedDescriptor,
    Changed,
    Clients,
    Closing,
    OtherTraffic,
}

impl Blocker {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::InvalidRequest => message!(conflict_invalid_request),
            Self::Systemctl => message!(conflict_systemctl),
            Self::Systemd => message!(conflict_systemd),
            Self::MissingUnit => message!(conflict_missing_unit),
            Self::Inactive => message!(conflict_inactive),
            Self::UnitIdentity => message!(conflict_unit_identity),
            Self::StopPolicy => message!(conflict_stop_policy),
            Self::Busctl => message!(conflict_busctl),
            Self::StartTime => message!(conflict_start_time),
            Self::ExeAccess => message!(conflict_exe_access),
            Self::Executable => message!(conflict_executable),
            Self::NamespaceAccess => message!(conflict_namespace_access),
            Self::NamespaceMismatch => message!(conflict_namespace_mismatch),
            Self::CgroupAccess => message!(conflict_cgroup_access),
            Self::CgroupShared => message!(conflict_cgroup_shared),
            Self::ProcessConfig => message!(conflict_process_config),
            Self::ConfigSource => message!(conflict_config_source),
            Self::ConfigAccess => message!(conflict_config_access),
            Self::ConfigIdentity => message!(conflict_config_identity),
            Self::ConfigChanged => message!(conflict_config_changed),
            Self::ConfigFormat => message!(conflict_config_format),
            Self::ConfigFields => message!(conflict_config_fields),
            Self::InboundPolicy => message!(conflict_inbound_policy),
            Self::AdditionalInbound => message!(conflict_additional_inbound),
            Self::Descriptors => message!(conflict_descriptors),
            Self::SocketTables => message!(conflict_socket_tables),
            Self::UnixListener => message!(conflict_unix_listener),
            Self::Udp => message!(conflict_udp),
            Self::ForeignListener => message!(conflict_foreign_listener),
            Self::OtherListener => message!(conflict_other_listener),
            Self::BindAddress => message!(conflict_bind_address),
            Self::SocketMismatch => message!(conflict_socket_mismatch),
            Self::ExclusiveAccess => message!(conflict_exclusive_access),
            Self::SharedDescriptor => message!(conflict_shared_descriptor),
            Self::Changed => message!(conflict_changed),
            Self::Clients => message!(conflict_clients),
            Self::Closing => message!(conflict_closing),
            Self::OtherTraffic => message!(conflict_other_traffic),
        }
    }
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
    fn add(&mut self, inbound: Inbound) -> Result<(), Blocker> {
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
        {
            return Err(Blocker::InboundPolicy);
        }
        if inbound.tag.is_some_and(|tag| !self.tags.insert(tag))
            || [self.http, self.socks].contains(&Some(inbound.port))
        {
            return Err(Blocker::AdditionalInbound);
        }
        let slot = match (inbound.protocol.as_str(), options.auth.as_deref()) {
            ("http", None) => &mut self.http,
            ("socks", None | Some("noauth")) => &mut self.socks,
            _ => return Err(Blocker::InboundPolicy),
        };
        slot.replace(inbound.port)
            .is_none()
            .then_some(())
            .ok_or(Blocker::AdditionalInbound)
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

fn output(program: &str, args: &[&str]) -> Option<std::process::Output> {
    Command::new(program)
        .args(args)
        .env_remove("DBUS_SYSTEM_BUS_ADDRESS")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
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

fn service() -> Result<Service, Blocker> {
    if !Path::new("/usr/bin/systemctl").is_file() {
        return Err(Blocker::Systemctl);
    }
    let properties = output(
        "/usr/bin/systemctl",
        &[
            "--system",
            "show",
            "--all",
            "xray.service",
            "-p",
            "Id,Names,LoadState,ActiveState,SubState,MainPID,ExecMainPID,ControlGroup",
            "-p",
            "RefuseManualStop,BoundBy,RequiredBy,ConsistsOf,PropagatesStopTo,TriggeredBy",
        ],
    )
    .ok_or(Blocker::Systemd)?;
    let fields: HashMap<_, _> = std::str::from_utf8(&properties.stdout)
        .map_err(|_| Blocker::Systemd)?
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    if fields.get("LoadState") == Some(&"not-found") {
        return Err(Blocker::MissingUnit);
    }
    if !properties.status.success() {
        return Err(Blocker::Systemd);
    }
    if fields.get("ActiveState") != Some(&"active") || fields.get("SubState") != Some(&"running") {
        return Err(Blocker::Inactive);
    }
    if fields.get("Id") != Some(&"xray.service")
        || fields.get("Names") != Some(&"xray.service")
        || fields.get("MainPID") != fields.get("ExecMainPID")
    {
        return Err(Blocker::UnitIdentity);
    }
    if !independently_stoppable(&fields) {
        return Err(Blocker::StopPolicy);
    }
    let pid: u32 = fields
        .get("MainPID")
        .and_then(|value| value.parse().ok())
        .filter(|pid| *pid > 1)
        .ok_or(Blocker::UnitIdentity)?;
    let group = Path::new(fields.get("ControlGroup").ok_or(Blocker::CgroupAccess)?);
    if !group.is_absolute()
        || group.file_name().is_none_or(|name| name != "xray.service")
        || !group
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(Blocker::CgroupAccess);
    }
    if !Path::new("/usr/bin/busctl").is_file() {
        return Err(Blocker::Busctl);
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
    )
    .ok_or(Blocker::StartTime)?;
    if !started.status.success() {
        return Err(Blocker::StartTime);
    }
    let started_us = std::str::from_utf8(&started.stdout)
        .map_err(|_| Blocker::StartTime)?
        .trim()
        .strip_prefix("t ")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .ok_or(Blocker::StartTime)?;
    Ok(Service {
        pid,
        started_us,
        group: group.to_owned(),
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

fn configuration(process: &Path, started_us: u64) -> Result<Ports, Blocker> {
    let env = fs::read(process.join("environ")).map_err(|_| Blocker::ProcessConfig)?;
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
        return Err(Blocker::ConfigSource);
    }
    let args = fs::read(process.join("cmdline")).map_err(|_| Blocker::ProcessConfig)?;
    let mut files = Vec::new();
    for (directory, path) in config_sources(&args).ok_or(Blocker::ConfigSource)? {
        let mut resolved = process.join("root");
        for part in path.components() {
            if let Component::Normal(name) = part {
                resolved.push(name);
                if fs::symlink_metadata(&resolved)
                    .map_err(|_| Blocker::ConfigAccess)?
                    .is_symlink()
                {
                    return Err(Blocker::ConfigIdentity);
                }
            }
        }
        // PrivateTmp and read-only bind mounts need not change the file that supplied the config.
        let host = fs::metadata(&path).map_err(|_| Blocker::ConfigAccess)?;
        let mounted = fs::metadata(&resolved).map_err(|_| Blocker::ConfigAccess)?;
        if (host.dev(), host.ino()) != (mounted.dev(), mounted.ino()) {
            return Err(Blocker::ConfigIdentity);
        }
        if directory {
            if !mounted.is_dir() {
                return Err(Blocker::ConfigFormat);
            }
            if !unchanged(&mounted, started_us) {
                return Err(Blocker::ConfigChanged);
            }
            for entry in fs::read_dir(&resolved).map_err(|_| Blocker::ConfigAccess)? {
                let entry = entry.map_err(|_| Blocker::ConfigAccess)?;
                files.push((path.join(entry.file_name()), entry.path()));
            }
        } else {
            files.push((path, resolved));
        }
    }
    let mut ports = Ports::default();
    for (path, resolved) in files {
        let metadata = fs::symlink_metadata(&resolved).map_err(|_| Blocker::ConfigAccess)?;
        let host = fs::symlink_metadata(&path).map_err(|_| Blocker::ConfigAccess)?;
        let identity = (host.dev(), host.ino());
        if !metadata.is_file() || (metadata.dev(), metadata.ino()) != identity {
            return Err(Blocker::ConfigIdentity);
        }
        if path.extension().is_none_or(|extension| extension != "json")
            || metadata.len() > MAX_CONFIG_BYTES
        {
            return Err(Blocker::ConfigFormat);
        }
        if !unchanged(&metadata, started_us) {
            return Err(Blocker::ConfigChanged);
        }
        let file = File::open(resolved).map_err(|_| Blocker::ConfigAccess)?;
        let observed = file.metadata().map_err(|_| Blocker::ConfigAccess)?;
        if (observed.dev(), observed.ino()) != identity {
            return Err(Blocker::ConfigIdentity);
        }
        let config: Config = serde_json::from_reader((&file).take(MAX_CONFIG_BYTES + 1)).map_err(
            |error| match error.classify() {
                serde_json::error::Category::Data => Blocker::ConfigFields,
                serde_json::error::Category::Io => Blocker::ConfigAccess,
                _ => Blocker::ConfigFormat,
            },
        )?;
        if !unchanged(
            &file.metadata().map_err(|_| Blocker::ConfigAccess)?,
            started_us,
        ) {
            return Err(Blocker::ConfigChanged);
        }
        for inbound in config.inbounds {
            ports.add(inbound)?;
        }
    }
    Ok(ports)
}

fn socket_inodes(process: &Path) -> Option<HashSet<u64>> {
    let mut result = HashSet::new();
    for entry in fs::read_dir(process.join("fd")).ok()? {
        let path = match fs::read_link(entry.ok()?.path()) {
            Ok(path) => path,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        if let Some(text) = path.to_str().and_then(|s| s.strip_prefix("socket:[")) {
            result.insert(text.strip_suffix(']')?.parse().ok()?);
        }
    }
    Some(result)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Address {
    Loopback,
    MappedLoopback,
    AnyV4,
    AnyV6,
    Elsewhere,
}

impl Address {
    fn from_proc(raw: &str) -> Option<Self> {
        // /proc/net formats each 32-bit word in native byte order, including tcp6.
        let word = |chunk: &[u8]| {
            u32::from_str_radix(std::str::from_utf8(chunk).ok()?, 16)
                .ok()
                .map(u32::to_ne_bytes)
        };
        let (v4, mapped) = match raw.len() {
            8 => (Ipv4Addr::from(word(raw.as_bytes())?), false),
            32 => {
                let mut bytes = [0; 16];
                for (chunk, out) in raw
                    .as_bytes()
                    .chunks_exact(8)
                    .zip(bytes.chunks_exact_mut(4))
                {
                    out.copy_from_slice(&word(chunk)?);
                }
                let ip = Ipv6Addr::from(bytes);
                if ip.is_unspecified() {
                    return Some(Self::AnyV6);
                }
                let Some(v4) = ip.to_ipv4_mapped() else {
                    return Some(Self::Elsewhere);
                };
                (v4, true)
            }
            _ => return None,
        };
        Some(match v4 {
            Ipv4Addr::UNSPECIFIED => Self::AnyV4,
            Ipv4Addr::LOCALHOST if mapped => Self::MappedLoopback,
            Ipv4Addr::LOCALHOST => Self::Loopback,
            _ => Self::Elsewhere,
        })
    }

    fn loopback(self) -> bool {
        matches!(self, Self::Loopback | Self::MappedLoopback)
    }

    fn accepts_v4_loopback(self) -> bool {
        self.loopback() || self == Self::AnyV4
    }
}

#[derive(Clone, Copy)]
struct Socket {
    inode: u64,
    port: u16,
    address: Address,
    listen: bool,
    udp: bool,
    active: bool,
    incoming: bool,
    closing: bool,
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
            let address = Address::from_proc(address)?;
            result.push(Socket {
                inode,
                port: u16::from_str_radix(port, 16).ok()?,
                address,
                active: !matches!(state, "06" | "07" | "0A"),
                incoming: matches!(state, "01" | "03" | "0C"),
                closing: matches!(state, "04" | "05" | "06" | "07" | "08" | "09" | "0B"),
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
) -> Result<Option<Blocker>, Blocker> {
    let mut found = HashSet::new();
    let mut clients = false;
    let mut closing = false;
    let mut other = false;
    for row in rows {
        let ours = owned.contains(&row.inode);
        let selected = requested.contains(&Some(row.port));
        let wanted = selected && row.address != Address::Elsewhere;
        if row.udp && (ours || wanted) {
            return Err(Blocker::Udp);
        }
        if row.listen && (ours || wanted) {
            if !ours {
                return Err(Blocker::ForeignListener);
            }
            if !wanted {
                return Err(Blocker::OtherListener);
            }
            if row.address != Address::Loopback {
                return Err(Blocker::BindAddress);
            }
            if ![declared.http, declared.socks].contains(&Some(row.port)) || !found.insert(row.port)
            {
                return Err(Blocker::SocketMismatch);
            }
        } else if row.active && !row.udp && (ours || wanted) {
            // Includes incoming connections queued for accept before a file descriptor exists.
            if selected && row.address.loopback() && row.incoming {
                clients = true;
            } else if selected && row.address.loopback() && row.closing {
                closing = true;
            } else {
                other = true;
            }
        }
    }
    if [declared.http, declared.socks]
        .into_iter()
        .flatten()
        .any(|port| !found.contains(&port))
    {
        return Err(Blocker::SocketMismatch);
    }
    Ok(if clients {
        Some(Blocker::Clients)
    } else if closing {
        Some(Blocker::Closing)
    } else if other {
        Some(Blocker::OtherTraffic)
    } else {
        None
    })
}

fn same_namespace(process: &Path) -> Result<(), Blocker> {
    let ours = fs::metadata("/proc/self/ns/net").map_err(|_| Blocker::NamespaceAccess)?;
    let theirs = fs::metadata(process.join("ns/net")).map_err(|_| Blocker::NamespaceAccess)?;
    ((ours.dev(), ours.ino()) == (theirs.dev(), theirs.ino()))
        .then_some(())
        .ok_or(Blocker::NamespaceMismatch)
}

// In a container the cgroup2 mount may expose a delegated subtree, with a different
// prefix from systemd's ControlGroup. Confirm candidates against cgroup.procs below.
fn cgroup_directories(group: &Path, mounts: &str) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for row in mounts.lines() {
        let Some((fields, filesystem)) = row.split_once(" - ") else {
            continue;
        };
        if !filesystem.starts_with("cgroup2 ") {
            continue;
        }
        let mut fields = fields.split_ascii_whitespace();
        let (Some(root), Some(mount)) = (fields.nth(3), fields.next()) else {
            continue;
        };
        if [root, mount]
            .iter()
            .any(|p| !Path::new(p).is_absolute() || p.contains('\\'))
        {
            continue;
        }
        for prefix in [Path::new(root), Path::new("/")] {
            if let Ok(suffix) = group.strip_prefix(prefix) {
                let dir = Path::new(mount).join(suffix);
                if !result.contains(&dir) {
                    result.push(dir);
                }
            }
        }
    }
    result
}

fn isolated_group(service: &Service) -> Result<(), Blocker> {
    let mounts = fs::read_to_string("/proc/self/mountinfo").map_err(|_| Blocker::CgroupAccess)?;
    for dir in cgroup_directories(&service.group, &mounts) {
        let Ok(members) = fs::read_to_string(dir.join("cgroup.procs")) else {
            continue;
        };
        let pid = service.pid.to_string();
        if !members.lines().any(|member| member == pid) {
            continue;
        }
        if members.trim() != pid {
            return Err(Blocker::CgroupShared);
        }
        for entry in fs::read_dir(dir).map_err(|_| Blocker::CgroupAccess)? {
            if entry
                .and_then(|p| p.file_type())
                .map_err(|_| Blocker::CgroupAccess)?
                .is_dir()
            {
                return Err(Blocker::CgroupShared);
            }
        }
        return Ok(());
    }
    Err(Blocker::CgroupAccess)
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

fn exclusively_held(pid: u32, listeners: &HashSet<u64>) -> Result<(), Blocker> {
    for entry in fs::read_dir("/proc").map_err(|_| Blocker::ExclusiveAccess)? {
        let entry = entry.map_err(|_| Blocker::ExclusiveAccess)?;
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
            Err(_) => return Err(Blocker::ExclusiveAccess),
        };
        for descriptor in descriptors {
            match fs::metadata(descriptor.map_err(|_| Blocker::ExclusiveAccess)?.path()) {
                Ok(metadata)
                    if metadata.file_type().is_socket() && listeners.contains(&metadata.ino()) =>
                {
                    return Err(Blocker::SharedDescriptor);
                }
                Ok(_) => (),
                Err(error) if error.kind() == ErrorKind::NotFound => (),
                Err(_) => return Err(Blocker::ExclusiveAccess),
            }
        }
    }
    Ok(())
}

fn valid_request(http: Option<u16>, socks: Option<u16>) -> bool {
    http != socks && http != Some(0) && socks != Some(0)
}

#[derive(Default)]
struct Traffic {
    rows: Option<Vec<Socket>>,
    other_flows: Option<usize>,
}

fn verified(
    http: Option<u16>,
    socks: Option<u16>,
    traffic: &mut Traffic,
) -> Result<(XrayListeners, Option<Blocker>), Blocker> {
    if !cfg!(target_os = "linux") || !valid_request(http, socks) {
        return Err(Blocker::InvalidRequest);
    }
    let first = service()?;
    let process = Path::new("/proc").join(first.pid.to_string());
    let exe = fs::read_link(process.join("exe")).map_err(|_| Blocker::ExeAccess)?;
    if exe.file_name().is_none_or(|name| name != "xray") {
        return Err(Blocker::Executable);
    }
    same_namespace(&process)?;
    isolated_group(&first)?;
    let config = configuration(&process, first.started_us)?;
    let owned = socket_inodes(&process).ok_or(Blocker::Descriptors)?;
    if other_listeners(&owned).ok_or(Blocker::SocketTables)? {
        return Err(Blocker::UnixListener);
    }
    let rows = traffic.rows.insert(sockets().ok_or(Blocker::SocketTables)?);
    let requested = [http, socks];
    traffic.other_flows = Some(
        rows.iter()
            .filter(|r| {
                !r.udp
                    && r.active
                    && owned.contains(&r.inode)
                    && (!requested.contains(&Some(r.port))
                        || !r.address.loopback()
                        || (!r.incoming && !r.closing))
            })
            .count(),
    );
    let busy = check_sockets(&owned, rows, &config, requested)?;
    let listeners = rows
        .iter()
        .filter(|r| r.listen && owned.contains(&r.inode))
        .map(|r| r.inode)
        .collect();
    exclusively_held(first.pid, &listeners)?;
    let (http, socks) = config.assign(http, socks).ok_or(Blocker::SocketMismatch)?;
    if service()? != first {
        return Err(Blocker::Changed);
    }
    Ok((
        XrayListeners {
            pid: first.pid,
            http,
            socks,
            busy: busy.is_some(),
            started_us: first.started_us,
        },
        busy,
    ))
}

pub(crate) fn inspect(http: Option<u16>, socks: Option<u16>) -> Option<XrayListeners> {
    verified(http, socks, &mut Traffic::default())
        .ok()
        .map(|(snapshot, _)| snapshot)
}

fn port_reports(requested: [Option<u16>; 2], rows: Option<&[Socket]>) -> Vec<PortReport> {
    requested
        .into_iter()
        .flatten()
        .map(|port| {
            let count = |filter: fn(&Socket) -> bool| {
                rows.map(|list| {
                    list.iter()
                        .filter(|r| r.port == port && !r.udp && filter(r))
                        .count()
                })
            };
            let listening = count(|r| r.listen && r.address.accepts_v4_loopback()).and_then(|n| {
                // The socket table does not expose IPV6_V6ONLY for an IPv6 wildcard.
                if n > 0 {
                    Some(true)
                } else if count(|r| r.listen && r.address == Address::AnyV6) == Some(0) {
                    Some(false)
                } else {
                    None
                }
            });
            PortReport {
                port,
                listening,
                owners: Vec::new(),
                clients: if listening == Some(false) {
                    Some(0)
                } else {
                    count(|r| r.address.loopback() && r.incoming)
                },
                closing: count(|r| r.address.loopback() && r.closing),
            }
        })
        .collect()
}

fn listener_owners(ports: &mut [PortReport], rows: &[Socket]) {
    let mut wanted = HashMap::new();
    for (index, port) in ports.iter().enumerate() {
        for row in rows.iter().filter(|r| {
            r.port == port.port && r.listen && r.address.accepts_v4_loopback() && r.inode != 0
        }) {
            wanted.insert(row.inode, index);
        }
    }
    if wanted.is_empty() {
        return;
    }
    let Ok(processes) = fs::read_dir("/proc") else {
        return;
    };
    for entry in processes.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        let process = entry.path();
        let Ok(descriptors) = fs::read_dir(process.join("fd")) else {
            continue;
        };
        let mut found = HashSet::new();
        for fd in descriptors.flatten() {
            if let Ok(meta) = fs::metadata(fd.path())
                && meta.file_type().is_socket()
                && let Some(index) = wanted.get(&meta.ino())
            {
                found.insert(*index);
            }
        }
        if !found.is_empty() {
            let name = fs::read_link(process.join("exe")).ok().and_then(|path| {
                let name = path.file_name()?.to_str()?;
                (name.len() <= 64
                    && name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)))
                .then(|| name.to_owned())
            });
            for index in found {
                ports[index].owners.push(Owner {
                    pid,
                    name: name.clone(),
                });
            }
        }
    }
    for port in ports {
        port.owners.sort_by_key(|owner| owner.pid);
    }
}

pub(crate) fn diagnose(http: Option<u16>, socks: Option<u16>) -> Inspection {
    let mut traffic = Traffic::default();
    let (listeners, blocker) = match verified(http, socks, &mut traffic) {
        Ok((snapshot, blocker)) => (Some(snapshot), blocker),
        Err(reason) => (None, Some(reason)),
    };
    // Use the same kernel snapshot as the eligibility check whenever it reached the socket stage.
    let rows = traffic.rows.or_else(sockets);
    let mut ports = port_reports([http, socks], rows.as_deref());
    if let Some(rows) = rows {
        listener_owners(&mut ports, &rows);
    }
    Inspection {
        ports,
        listeners,
        blocker,
        other_flows: traffic.other_flows,
    }
}

pub(crate) fn owns_listeners(pid: u32, http: Option<u16>, socks: Option<u16>) -> bool {
    fn check(pid: u32, http: Option<u16>, socks: Option<u16>) -> Option<bool> {
        if !cfg!(target_os = "linux") || !valid_request(http, socks) {
            return Some(false);
        }
        let process = Path::new("/proc").join(pid.to_string());
        let owned = socket_inodes(&process)?;
        same_namespace(&process).ok()?;
        let rows = sockets()?;
        Some([http, socks].into_iter().flatten().all(|port| {
            let mut matching = rows.iter().filter(|row| row.listen && row.port == port);
            matching
                .next()
                .is_some_and(|row| row.address == Address::Loopback && owned.contains(&row.inode))
                && matching.next().is_none()
        }))
    }
    check(pid, http, socks) == Some(true)
}

#[cfg(test)]
#[path = "../tests/unit/xray_listeners.rs"]
mod tests;
