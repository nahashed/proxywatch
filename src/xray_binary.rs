use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const PRIVATE_XRAY: &str = "/usr/local/lib/proxywatch/xray";
const STANDARD_BINARIES: [&str; 3] = [PRIVATE_XRAY, "/usr/local/bin/xray", "/usr/bin/xray"];
const MINIMUM_VERSION: [u32; 3] = [26, 7, 28];

fn executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|info| info.is_file() && info.permissions().mode() & 0o111 != 0)
}

pub fn default_binary() -> &'static str {
    STANDARD_BINARIES
        .into_iter()
        .find(|path| executable(Path::new(path)))
        .unwrap_or("xray")
}

pub fn setup_binary() -> String {
    env::var("PROXYWATCH_SETUP_XRAY")
        .ok()
        .filter(|value| Path::new(value).is_absolute() && executable(Path::new(value)))
        .unwrap_or_else(|| default_binary().into())
}

pub(crate) fn version_number(output: &[u8]) -> Option<[u32; 3]> {
    let mut fields = std::str::from_utf8(output)
        .ok()?
        .lines()
        .next()?
        .split_ascii_whitespace();
    if fields.next()? != "Xray" {
        return None;
    }
    let mut numbers = fields.next()?.split('.');
    let version = [
        numbers.next()?.parse().ok()?,
        numbers.next()?.parse().ok()?,
        numbers.next()?.parse().ok()?,
    ];
    numbers.next().is_none().then_some(version)
}

pub fn inspect_binary(configured: &str) -> Option<(PathBuf, [u32; 3])> {
    select_binary(configured, &STANDARD_BINARIES, probe_binary)
}

// Old settings can name the system Xray even after the installer added a newer private copy.
fn select_binary<T>(
    configured: &str,
    standard: &[&str],
    mut probe: impl FnMut(&str) -> Option<T>,
) -> Option<T> {
    if configured == "xray" {
        return standard
            .iter()
            .find_map(|path| probe(path))
            .or_else(|| probe(configured));
    }
    probe(configured).or_else(|| {
        if standard.contains(&configured) {
            standard
                .iter()
                .filter(|&&path| path != configured)
                .find_map(|path| probe(path))
        } else {
            None
        }
    })
}

pub fn select_for_run(configured: &mut String) {
    if (configured == "xray" || STANDARD_BINARIES.contains(&configured.as_str()))
        && let Some((path, _)) = inspect_binary(configured)
        && let Some(path) = path.to_str()
    {
        *configured = path.to_owned();
    }
}

fn probe_binary(configured: &str) -> Option<(PathBuf, [u32; 3])> {
    let path = if configured.contains('/') {
        std::path::absolute(configured).ok()?
    } else {
        let search = env::var_os("PATH")?;
        let candidate = env::split_paths(&search)
            .map(|path| path.join(configured))
            .find(|path| executable(path))?;
        std::path::absolute(candidate).ok()?
    };
    // A user-supplied executable might not implement the version command.
    let mut command = if cfg!(target_os = "linux") {
        let mut command = Command::new("/usr/bin/timeout");
        command.args(["-k", "1", "4"]).arg(&path);
        command
    } else {
        Command::new(&path)
    };
    let output = command
        .arg("version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let version = version_number(&output.stdout)?;
    (output.status.success() && version >= MINIMUM_VERSION).then_some((path, version))
}

#[cfg(test)]
#[path = "../tests/unit/xray_binary.rs"]
mod tests;
