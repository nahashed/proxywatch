use super::*;
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    tmp: tempfile::TempDir,
    systemctl: PathBuf,
    config: PathBuf,
    user: PathBuf,
    system: PathBuf,
}

impl Fixture {
    fn new() -> std::io::Result<Self> {
        let tmp = tempfile::tempdir()?;
        let base = tmp.path();
        fs::set_permissions(base, fs::Permissions::from_mode(0o700))?;
        let systemctl = base.join("systemctl");
        let config = base.join("proxywatch");
        fs::create_dir(&config)?;
        let user = base.join("user.service");
        let system = base.join("system.service");
        fs::write(&user, include_bytes!("../../deploy/proxywatch.service"))?;
        fs::write(
            &system,
            include_bytes!("../../deploy/system/proxywatch.service"),
        )?;
        fs::write(base.join("offline"), "disabled")?;
        fs::write(
            &systemctl,
            r#"#!/bin/sh
base=${0%/*}
scope=${1#--}
shift
printf '%s %s\n' "$scope" "$*" >> "$base/calls"
case "$1" in
    show-environment) echo "XDG_CONFIG_HOME=$base" ;;
    --root=/) offline=$(cat "$base/offline"); echo "$offline"; [ "$offline" = enabled ] ;;
    show)
        if [ "$2" = --property=MainPID ]; then echo 42; exit 0; fi
        printf 'LoadState=loaded\nFragmentPath=%s/%s.service\n' "$base" "$scope"
        cat "$base/$scope.state"
        ;;
    start|stop|enable|try-restart|is-active|reset-failed) exit 0 ;;
    *) exit 1 ;;
esac
"#,
        )?;
        fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755))?;
        let fixture = Self {
            tmp,
            config,
            systemctl,
            user,
            system,
        };
        for scope in ["user", "system"] {
            fixture.state(scope, "inactive", "disabled", "")?;
        }
        Ok(fixture)
    }

    fn state(
        &self,
        scope: &str,
        active: &str,
        enabled: &str,
        dropins: &str,
    ) -> std::io::Result<()> {
        fs::write(
            self.tmp.path().join(format!("{scope}.state")),
            format!(
                "ActiveState={active}\nUnitFileState={enabled}\nDropInPaths={dropins}\nNeedDaemonReload=no\n"
            ),
        )
    }

    fn manager(&self) -> std::io::Result<std::os::unix::net::UnixListener> {
        std::os::unix::net::UnixListener::bind(self.tmp.path().join("bus"))
    }

    fn discover(&self, root_default: bool) -> Option<UserService<'_>> {
        UserService::discover(Installation {
            systemctl: &self.systemctl,
            runtime: self.tmp.path().to_owned(),
            directory: self.config.clone(),
            uid: fs::metadata(self.tmp.path()).unwrap().uid(),
            user_fragment: &self.user,
            system_fragment: root_default.then_some(&self.system),
        })
    }
}

#[test]
fn system_fallback_respects_saved_state_foreground_lock_and_offline_user_enablement()
-> std::io::Result<()> {
    let fixture = Fixture::new()?;
    assert!(fixture.discover(false).is_none());
    let service = fixture.discover(true).unwrap();
    assert_eq!(service.scope(), Scope::System);
    assert!(service.inactive_disabled());
    assert!(matches!(service.apply(false), Applied::Inactive));
    let guard = fs::File::create(fixture.config.join("daemon.lock"))?;
    fs2::FileExt::try_lock_exclusive(&guard)?;
    assert!(!service.start());
    assert!(matches!(service.apply(true), Applied::Failed));
    drop(guard);
    assert!(service.start());
    assert!(matches!(service.apply(true), Applied::Started));
    fs::write(fixture.tmp.path().join("offline"), "enabled")?;
    assert!(fixture.discover(true).is_none());
    assert!(matches!(service.apply(true), Applied::Failed));
    let calls = fs::read_to_string(fixture.tmp.path().join("calls"))?;
    assert_eq!(calls.matches("system start proxywatch.service").count(), 1);
    assert_eq!(
        calls
            .matches("system enable --now proxywatch.service")
            .count(),
        1
    );
    Ok(())
}

#[test]
fn prefers_existing_system_enablement_then_user_and_refuses_two_claims() -> std::io::Result<()> {
    let fixture = Fixture::new()?;
    let _bus = fixture.manager()?;
    let service = fixture.discover(true).unwrap();
    assert_eq!(service.scope(), Scope::User);
    assert!(matches!(service.apply(true), Applied::Started));
    fixture.state("user", "active", "disabled", "")?;
    assert!(matches!(service.apply(false), Applied::Restarted));
    fixture.state("system", "inactive", "enabled", "")?;
    assert!(fixture.discover(true).is_none());
    assert!(matches!(service.apply(false), Applied::Failed));
    fixture.state("user", "inactive", "disabled", "")?;
    assert_eq!(fixture.discover(true).unwrap().scope(), Scope::System);
    assert!(!service.start());
    fixture.state("system", "active", "disabled", "")?;
    assert_eq!(fixture.discover(true).unwrap().scope(), Scope::System);
    Ok(())
}

#[test]
fn rejects_changed_fragments_dropins_and_foreign_running_units() -> std::io::Result<()> {
    let fixture = Fixture::new()?;
    let system = fixture.discover(true).unwrap();
    fixture.state("system", "inactive", "disabled", "/etc/override.conf")?;
    assert!(!system.start());
    assert!(!system.stop());
    assert!(fixture.discover(true).is_none());
    let _bus = fixture.manager()?;
    assert_eq!(fixture.discover(true).unwrap().scope(), Scope::User);
    fixture.state("system", "active", "disabled", "/etc/override.conf")?;
    assert!(fixture.discover(true).is_none());
    fixture.state("system", "inactive", "disabled", "")?;
    let mut other_directory = fixture.discover(true).unwrap().installation;
    other_directory.directory = fixture.tmp.path().to_owned();
    assert!(UserService::discover(other_directory).is_none());
    fs::write(&fixture.user, "ExecStart=/bin/true")?;
    assert!(fixture.discover(true).is_none());
    fs::write(&fixture.system, "ExecStart=/bin/false")?;
    assert!(fixture.discover(true).is_none());
    Ok(())
}

#[test]
fn foreground_commands_preserve_custom_directory_and_quoting() {
    let command = foreground_command(Some(Path::new("/tmp/my 'profile")));
    assert!(command.ends_with(r" --directory '/tmp/my '\''profile' run"));
    assert!(!foreground_command(None).contains("--directory"));
}

#[test]
fn trusts_a_private_runtime_directory_and_its_owners_unix_socket() -> std::io::Result<()> {
    let tmp = tempfile::tempdir()?;
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o700))?;
    let uid = fs::metadata(tmp.path())?.uid();
    assert!(!has_owner_socket(tmp.path(), uid));
    let _listener = std::os::unix::net::UnixListener::bind(tmp.path().join("bus"))?;
    assert!(has_owner_socket(tmp.path(), uid));
    assert!(!has_owner_socket(tmp.path(), uid.wrapping_add(1)));
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o755))?;
    assert!(!has_owner_socket(tmp.path(), uid));
    Ok(())
}

#[test]
fn saving_can_recover_an_enabled_unit_after_it_hits_the_start_limit() -> std::io::Result<()> {
    let fixture = Fixture::new()?;
    fixture.state("system", "failed", "enabled", "")?;
    let service = fixture.discover(true).unwrap();
    assert!(matches!(service.apply(false), Applied::Started));
    let calls = fs::read_to_string(fixture.tmp.path().join("calls"))?;
    assert!(
        calls
            .find("system reset-failed proxywatch.service")
            .unwrap()
            < calls.find("system start proxywatch.service").unwrap()
    );
    assert!(!calls.contains("system enable --now"));
    Ok(())
}
