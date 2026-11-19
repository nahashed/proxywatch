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
const FRAGMENT: &str = "/usr/local/lib/systemd/user/proxywatch.service";

pub enum Applied {
    Started,
    Restarted,
    Inactive,
    Failed,
}

pub struct UserService<'a> {
    systemctl: &'a Path,
    runtime: PathBuf,
    active: bool,
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
        let runtime = PathBuf::from(format!("/run/user/{uid}"));
        if !has_owner_socket(&runtime, uid) {
            return None;
        }
        Self::discover(
            Path::new("/usr/bin/systemctl"),
            directory,
            Path::new(FRAGMENT),
            &runtime,
        )
    }
}

impl<'a> UserService<'a> {
    fn discover(
        systemctl: &'a Path,
        directory: &Path,
        fragment: &Path,
        runtime: &Path,
    ) -> Option<Self> {
        let control = |args: &[&str]| invoke(systemctl, runtime, args);
        let environment = control(&["show-environment"])?;
        // A shell and its user manager can resolve XDG_CONFIG_HOME differently.
        if manager_directory(&environment.stdout)?
            .canonicalize()
            .ok()?
            != directory.canonicalize().ok()?
        {
            return None;
        }
        control(&["daemon-reload"])?;
        let info = control(&[
            "show",
            "--all",
            "-p",
            "FragmentPath",
            "-p",
            "DropInPaths",
            "-p",
            "ActiveState",
            UNIT,
        ])?;
        let properties = std::str::from_utf8(&info.stdout).ok()?;
        let property = |prefix| {
            properties
                .lines()
                .find_map(|line| line.strip_prefix(prefix))
        };
        if !property("DropInPaths=")?.is_empty()
            || Path::new(property("FragmentPath=")?).canonicalize().ok()?
                != fragment.canonicalize().ok()?
            || fs::read(fragment).ok()? != include_bytes!("../deploy/proxywatch.service")
        {
            return None;
        }
        let active = matches!(
            property("ActiveState=")?,
            "active" | "activating" | "reloading"
        );
        Some(Self {
            systemctl,
            runtime: runtime.to_owned(),
            active,
        })
    }

    pub(crate) fn stopped(&self) -> bool {
        self.control(&["show", "--property=ActiveState", "--value", UNIT])
            .is_some_and(|output| {
                output.stdout.trim_ascii() == b"inactive" || output.stdout.trim_ascii() == b"failed"
            })
    }

    pub(crate) fn start(&self) -> bool {
        self.control(&["start", UNIT]).is_some()
    }

    pub(crate) fn stop(&self) -> bool {
        self.control(&["stop", UNIT]).is_some()
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
        invoke(self.systemctl, &self.runtime, args)
    }

    pub fn apply(&self, first_setup: bool) -> Applied {
        let (args, success) = if first_setup {
            (&["enable", "--now", UNIT][..], Applied::Started)
        } else if self.active {
            (&["try-restart", UNIT][..], Applied::Restarted)
        } else {
            return Applied::Inactive;
        };
        if invoke(self.systemctl, &self.runtime, args).is_some()
            && invoke(
                self.systemctl,
                &self.runtime,
                &["is-active", "--quiet", UNIT],
            )
            .is_some()
        {
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

fn invoke(systemctl: &Path, runtime: &Path, args: &[&str]) -> Option<Output> {
    let output = Command::new(systemctl)
        .env("XDG_RUNTIME_DIR", runtime)
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .arg("--user")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(output)
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
