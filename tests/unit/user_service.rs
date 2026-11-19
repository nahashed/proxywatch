use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn starts_and_restarts_only_the_matching_user_installation() -> std::io::Result<()> {
    let tmp = tempfile::tempdir()?;
    let config = tmp.path().join("proxywatch");
    let unit = tmp.path().join(UNIT);
    let systemctl = tmp.path().join("systemctl");
    let log = tmp.path().join("calls");
    let active = tmp.path().join("state");
    fs::create_dir(&config)?;
    fs::write(&unit, include_bytes!("../../deploy/proxywatch.service"))?;
    fs::write(&active, "inactive")?;
    fs::write(
        &systemctl,
        format!(
            "#!/bin/sh\n[ \"$1\" = --user ] || exit 1\nshift\nprintf '%s\\n' \"$*\" >> '{log}'\ncase $1 in\nshow-environment) echo 'XDG_CONFIG_HOME={base}' ;;\nshow) printf 'FragmentPath=%s\\nDropInPaths=\\nActiveState=%s\\n' '{unit}' \"$(cat '{active}')\" ;;\nesac\n",
            log = log.display(),
            base = tmp.path().display(),
            unit = unit.display(),
            active = active.display()
        ),
    )?;
    fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755))?;
    let discover = || UserService::discover(&systemctl, &config, &unit).unwrap();
    assert!(matches!(discover().apply(true), Applied::Started));
    assert!(matches!(discover().apply(false), Applied::Inactive));
    fs::write(&active, "active")?;
    assert!(matches!(discover().apply(false), Applied::Restarted));
    let calls = fs::read_to_string(&log)?;
    assert_eq!(calls.matches("enable --now proxywatch.service").count(), 1);
    assert_eq!(calls.matches("try-restart proxywatch.service").count(), 1);
    assert!(UserService::discover(&systemctl, tmp.path(), &unit).is_none());
    fs::write(&unit, "[Service]\nExecStart=/bin/false\n")?;
    assert!(UserService::discover(&systemctl, &config, &unit).is_none());
    Ok(())
}

#[test]
fn foreground_commands_preserve_custom_directory_and_quoting() {
    let command = foreground_command(Some(Path::new("/tmp/my 'profile")));
    assert!(command.ends_with(r" --directory '/tmp/my '\''profile' run"));
    assert!(!foreground_command(None).contains("--directory"));
}
