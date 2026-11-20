use std::{
    env, fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt},
    },
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const PROGRAM: &str = "/usr/local/bin/proxywatch";
const UNIT: &str = "proxywatch.service";
const USER_FRAGMENT: &str = "/usr/local/lib/systemd/user/proxywatch.service";
const SYSTEM_FRAGMENT: &str = "/usr/local/lib/systemd/system/proxywatch.service";
const ROOT_DIRECTORY: &str = "/root/.config/proxywatch";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Scope {
    User,
    System,
}

pub enum Applied {
    Started,
    Restarted,
    Inactive,
    Failed,
}

pub struct UserService<'a> {
    installation: Installation<'a>,
    scope: Scope,
}

struct Installation<'a> {
    systemctl: &'a Path,
    runtime: PathBuf,
    directory: PathBuf,
    uid: u32,
    user_fragment: &'a Path,
    system_fragment: Option<&'a Path>,
}

#[derive(Clone, Copy)]
struct State {
    active: bool,
    retrying: bool,
    failed: bool,
    enabled: bool,
}

#[derive(Clone, Copy)]
enum Candidate {
    Missing,
    Foreign { claimed: bool },
    Verified(State),
}

impl Candidate {
    fn claimed(self) -> bool {
        match self {
            Self::Missing => false,
            Self::Foreign { claimed } => claimed,
            Self::Verified(state) => state.active || state.enabled,
        }
    }
}

impl UserService<'static> {
    pub fn installed(directory: &Path, explicit_directory: bool) -> Option<Self> {
        if explicit_directory
            || !cfg!(target_os = "linux")
            || env::current_exe().ok()?.as_path() != Path::new(PROGRAM)
        {
            return None;
        }
        let uid = fs::metadata("/proc/self").ok()?.uid();
        Self::discover(Installation {
            systemctl: Path::new("/usr/bin/systemctl"),
            runtime: PathBuf::from(format!("/run/user/{uid}")),
            directory: directory.to_owned(),
            uid,
            user_fragment: Path::new(USER_FRAGMENT),
            system_fragment: (uid == 0 && directory == Path::new(ROOT_DIRECTORY))
                .then(|| Path::new(SYSTEM_FRAGMENT)),
        })
    }
}

impl Installation<'_> {
    fn probe(&self, scope: Scope, fragment: &Path) -> Candidate {
        let unknown = Candidate::Foreign { claimed: true };
        let Some(info) = self.command(scope, &[
            "show", "--all",
            "--property=LoadState,FragmentPath,DropInPaths,ActiveState,SubState,MainPID,UnitFileState,NeedDaemonReload",
            UNIT,
        ]) else {
            return unknown;
        };
        let Ok(properties) = std::str::from_utf8(&info.stdout) else {
            return unknown;
        };
        let property = |prefix| {
            properties
                .lines()
                .find_map(|line| line.strip_prefix(prefix))
        };
        if property("LoadState=") == Some("not-found")
            && matches!(property("ActiveState="), Some("inactive" | "failed"))
        {
            return Candidate::Missing;
        }
        let (Some(active), Some(enabled)) = (property("ActiveState="), property("UnitFileState="))
        else {
            return unknown;
        };
        let state = State {
            active: matches!(active, "active" | "activating" | "reloading"),
            retrying: active == "activating"
                && property("MainPID=") == Some("0")
                && matches!(
                    property("SubState="),
                    Some("auto-restart" | "auto-restart-queued")
                ),
            failed: active == "failed",
            enabled: matches!(enabled, "enabled" | "enabled-runtime"),
        };
        let expected: &[u8] = match scope {
            Scope::User => include_bytes!("../deploy/proxywatch.service"),
            Scope::System => include_bytes!("../deploy/system/proxywatch.service"),
        };
        if info.status.success()
            && property("LoadState=") == Some("loaded")
            && property("DropInPaths=") == Some("")
            && property("NeedDaemonReload=") == Some("no")
            && property("FragmentPath=").map(Path::new) == Some(fragment)
            && fs::symlink_metadata(fragment).is_ok_and(|metadata| metadata.is_file())
            && fs::read(fragment).is_ok_and(|bytes| bytes == expected)
            && (state.active || matches!(active, "inactive" | "failed"))
            && (state.enabled || enabled == "disabled")
        {
            Candidate::Verified(state)
        } else {
            Candidate::Foreign {
                claimed: !matches!(active, "inactive" | "failed")
                    || !matches!(enabled, "disabled" | "masked" | "masked-runtime" | "static"),
            }
        }
    }

    fn user(&self) -> Candidate {
        if has_owner_socket(&self.runtime, self.uid)
            && let Some(environment) = self
                .command(Scope::User, &["show-environment"])
                .filter(|output| output.status.success())
        {
            let user = self.probe(Scope::User, self.user_fragment);
            if manager_directory(&environment.stdout)
                .and_then(|directory| directory.canonicalize().ok())
                .is_some_and(|directory| {
                    self.directory.canonicalize().ok().as_ref() == Some(&directory)
                })
            {
                return user;
            }
            return Candidate::Foreign {
                claimed: user.claimed(),
            };
        }
        // An enabled user unit can start on the next login even without a user bus now.
        if self.system_fragment.is_some() {
            let offline = self.command(Scope::User, &["--root=/", "is-enabled", UNIT]);
            if !offline.is_some_and(|output| {
                matches!(
                    output.stdout.trim_ascii(),
                    b"disabled" | b"masked" | b"masked-runtime" | b"static" | b"not-found"
                )
            }) {
                return Candidate::Foreign { claimed: true };
            }
        }
        Candidate::Missing
    }

    fn selected(&self) -> Option<(Scope, State)> {
        let user = self.user();
        let system = self
            .system_fragment
            .map_or(Candidate::Missing, |path| self.probe(Scope::System, path));
        match (user, system) {
            (Candidate::Foreign { claimed: true }, _)
            | (_, Candidate::Foreign { claimed: true }) => None,
            _ if user.claimed() && system.claimed() => None,
            (_, Candidate::Verified(state)) if system.claimed() => Some((Scope::System, state)),
            (Candidate::Verified(state), _) => Some((Scope::User, state)),
            (Candidate::Missing, Candidate::Verified(state)) => Some((Scope::System, state)),
            _ => None,
        }
    }

    fn command(&self, scope: Scope, args: &[&str]) -> Option<Output> {
        let mut command = Command::new(self.systemctl);
        command
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env_remove("DBUS_SYSTEM_BUS_ADDRESS");
        match scope {
            Scope::User => {
                command.env("XDG_RUNTIME_DIR", &self.runtime).arg("--user");
            }
            Scope::System => {
                command.arg("--system");
            }
        }
        command
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
    }

    fn daemon_lock_available(&self) -> bool {
        match fs::File::options()
            .read(true)
            .write(true)
            .open(self.directory.join("daemon.lock"))
        {
            Ok(file) => fs2::FileExt::try_lock_exclusive(&file).is_ok(),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
    }
}

impl<'a> UserService<'a> {
    fn discover(installation: Installation<'a>) -> Option<Self> {
        let (scope, _) = installation.selected()?;
        Some(Self {
            installation,
            scope,
        })
    }

    pub(crate) fn scope(&self) -> Scope {
        self.scope
    }

    fn state(&self) -> Option<State> {
        self.installation
            .selected()
            .and_then(|(scope, state)| (scope == self.scope).then_some(state))
    }

    pub(crate) fn stopped(&self) -> bool {
        self.state().is_some_and(|state| !state.active)
    }

    pub(crate) fn inactive_disabled(&self) -> bool {
        self.state()
            .is_some_and(|state| !state.active && !state.failed && !state.enabled)
    }

    pub(crate) fn available_for_handover(&self) -> bool {
        self.state()
            .is_some_and(|state| !state.active || state.retrying)
    }

    pub(crate) fn prepare_handover(&self) -> bool {
        // Cancel a pending restart before Xray releases the ports. reset-failed also
        // clears start-limit counters that may still apply after a successful stop.
        let Some(state) = self.state() else {
            return false;
        };
        if state.retrying {
            if !self.stop() {
                return false;
            }
            // A stopped, disabled unit can already have been unloaded along with its counters.
            let _ = self.control(&["reset-failed", UNIT]);
        } else if state.failed && self.control(&["reset-failed", UNIT]).is_none() {
            return false;
        }
        self.stopped()
    }

    pub(crate) fn active(&self) -> bool {
        self.state()
            .is_some_and(|state| state.active && !state.retrying)
    }

    pub(crate) fn start(&self) -> bool {
        self.stopped()
            && self.installation.daemon_lock_available()
            && self.control(&["start", UNIT]).is_some()
    }

    pub(crate) fn stop(&self) -> bool {
        self.state().is_some() && self.control(&["stop", UNIT]).is_some()
    }

    pub(crate) fn pid(&self) -> Option<u32> {
        let info = self.control(&["show", "--property=MainPID", "--value", UNIT])?;
        std::str::from_utf8(&info.stdout)
            .ok()?
            .trim()
            .parse()
            .ok()
            .filter(|pid| *pid > 0)
    }

    fn control(&self, args: &[&str]) -> Option<Output> {
        self.installation
            .command(self.scope, args)
            .filter(|output| output.status.success())
    }

    pub fn apply(&self, first_setup: bool) -> Applied {
        let Some(state) = self.state() else {
            return Applied::Failed;
        };
        let recovering = (state.failed || state.retrying) && (state.enabled || first_setup);
        if recovering && (!self.installation.daemon_lock_available() || !self.prepare_handover()) {
            return Applied::Failed;
        }
        let (args, success) = if first_setup {
            if !state.active && !self.installation.daemon_lock_available() {
                return Applied::Failed;
            }
            (&["enable", "--now", UNIT][..], Applied::Started)
        } else if recovering {
            (&["start", UNIT][..], Applied::Started)
        } else if state.active {
            (&["try-restart", UNIT][..], Applied::Restarted)
        } else {
            return Applied::Inactive;
        };
        if self.control(args).is_some() && self.control(&["is-active", "--quiet", UNIT]).is_some() {
            success
        } else {
            Applied::Failed
        }
    }
}

fn has_owner_socket(runtime: &Path, uid: u32) -> bool {
    if !fs::symlink_metadata(runtime)
        .is_ok_and(|m| m.is_dir() && m.uid() == uid && m.mode() & 0o777 == 0o700)
    {
        return false;
    }
    ["bus", "systemd/private"].iter().any(|name| {
        fs::symlink_metadata(runtime.join(name))
            .is_ok_and(|m| m.file_type().is_socket() && m.uid() == uid)
    })
}

fn manager_directory(environment: &[u8]) -> Option<PathBuf> {
    let variable = |name: &[u8]| {
        environment.split(|byte| *byte == b'\n').find_map(|line| {
            line.strip_prefix(name)
                .map(|value| Path::new(std::ffi::OsStr::from_bytes(value)))
        })
    };
    // systemctl quotes nontrivial values. Only unambiguous absolute paths qualify.
    let base = match variable(b"XDG_CONFIG_HOME=") {
        Some(base) => base.to_owned(),
        None => variable(b"HOME=")?.join(".config"),
    };
    base.is_absolute().then(|| base.join("proxywatch"))
}

pub fn foreground_command(directory: Option<&Path>) -> String {
    fn quote(path: &Path) -> String {
        if let Some(text) = path.to_str()
            && !text.chars().any(char::is_control)
        {
            return format!("'{}'", text.replace('\'', "'\\''"));
        }
        // Bash byte escapes also preserve non-UTF-8 directory names.
        format!("$'{}'", path.as_os_str().as_bytes().escape_ascii())
    }
    let program = env::current_exe().map_or_else(|_| "proxywatch".into(), |path| quote(&path));
    match directory {
        Some(directory) => format!("{program} --directory {} run", quote(directory)),
        None => format!("{program} run"),
    }
}

#[cfg(test)]
#[path = "../tests/unit/user_service.rs"]
mod tests;
