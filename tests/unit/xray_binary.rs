use super::*;

#[test]
fn reads_only_a_complete_xray_version_from_the_banner() {
    assert_eq!(
        version_number(b"Xray 26.7.28 (Xray, Penetrates Everything.)\n"),
        Some(MINIMUM_VERSION)
    );
    assert_eq!(version_number(b"Xray 26.8.1"), Some([26, 8, 1]));
    assert_eq!(version_number(b"xray-tools 26.11.19"), None);
    assert_eq!(version_number(b"Xray 26.7"), None);
    assert_eq!(version_number(b"Xray 26.7.28.1"), None);
}

#[test]
fn checks_a_selected_executable_and_ignores_its_unrelated_output() -> std::io::Result<()> {
    let dir = tempfile::tempdir()?;
    let binary = dir.path().join("xray");
    for (banner, supported) in [("Xray 26.7.27", false), ("Xray 26.7.28 (...) extra", true)] {
        fs::write(
            &binary,
            format!("#!/bin/sh\n[ \"$1\" = version ] || exit 1\nprintf '%s\\n' '{banner}'\n"),
        )?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
        assert_eq!(
            inspect_binary(binary.to_str().unwrap()).is_some(),
            supported
        );
    }
    Ok(())
}
