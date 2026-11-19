use super::*;

fn ports(http: Option<u16>, socks: Option<u16>) -> Ports {
    Ports {
        http,
        socks,
        ..Ports::default()
    }
}

#[test]
fn preserves_occupied_protocol_and_reassigns_the_free_port() {
    let requested = (Some(10809), Some(10808));
    for (declared, expected) in [
        (ports(Some(10809), None), requested),
        (ports(None, Some(10808)), requested),
        (ports(Some(10808), None), (Some(10808), Some(10809))),
        (ports(None, Some(10809)), (Some(10808), Some(10809))),
        (ports(Some(10808), Some(10809)), (Some(10808), Some(10809))),
    ] {
        assert_eq!(declared.assign(requested.0, requested.1), Some(expected));
    }
    assert_eq!(
        ports(Some(80), None).assign(None, Some(80)),
        Some((Some(80), None))
    );
    assert_eq!(
        ports(None, Some(80)).assign(Some(80), None),
        Some((None, Some(80)))
    );
    assert_eq!(ports(None, None).assign(Some(80), None), None);
    assert_eq!(ports(Some(80), Some(81)).assign(Some(80), None), None);
}

#[test]
fn only_explicit_local_plain_unauthenticated_tcp_inbounds_qualify() {
    use serde_json::json;
    let base = json!({"listen":"127.0.0.1","port":10808,"protocol":"socks","settings":{"auth":"noauth","udp":false}});
    let valid = |value| {
        serde_json::from_value(value)
            .ok()
            .and_then(|inbound| Ports::default().add(inbound))
            .is_some()
    };
    assert!(valid(base.clone()));
    for (key, bad) in [
        ("listen", json!("0.0.0.0")),
        ("listen", json!("::1")),
        ("listen", json!("192.0.2.1")),
        ("port", json!("10808-10900")),
        ("protocol", json!("mixed")),
        ("protocol", json!("dokodemo-door")),
        ("settings", json!({"auth":"password"})),
        ("settings", json!({"udp":true})),
        ("settings", json!({"accounts":[{}]})),
        ("settings", json!({"allowTransparent":true})),
        ("settings", json!({"userLevel":1})),
        ("streamSettings", json!({"security":"tls"})),
        (
            "streamSettings",
            json!({"sockopt":{"acceptProxyProtocol":true}}),
        ),
        ("sniffing", json!({"unknown":true})),
        ("allocate", json!({})),
    ] {
        let mut input = base.clone();
        input[key] = bad;
        assert!(!valid(input), "{key}");
    }
    assert!(serde_json::from_value::<Config>(json!({"inbounds":[{"listen":"127.0.0.1","port":10808,"protocol":"socks","sniffing":{"enabled":true,"destOverride":["tls","http"],"routeOnly":true}}],"routing":{"rules":[]},"outbounds":[{},{}],"dns":{"servers":["localhost"]},"policy":{}})).is_ok());
    for field in ["api", "observatory", "reverse", "metrics"] {
        assert!(serde_json::from_value::<Config>(json!({field:{}})).is_err());
    }
}

#[test]
fn rejects_missing_or_implicit_configuration_sources() {
    for args in [
        &b"xray\0run\0"[..],
        b"xray\0-c\0stdin:\0",
        b"xray\0-c\0relative.json\0",
        b"xray\0-config\0https://example.invalid/config\0",
        b"xray\0d\0/etc\0",
        b"xray\0run\0-c\0/etc/x.json\0-format\0yaml\0",
    ] {
        assert!(config_sources(args).is_none());
    }
    assert_eq!(
        config_sources(b"xray\0run\0-c=/etc/in.json\0-confdir\0/etc/xray.d\0"),
        Some(vec![
            (false, "/etc/in.json".into()),
            (true, "/etc/xray.d".into())
        ])
    );
}

#[test]
fn distinguishes_owned_listeners_from_other_sockets_and_active_traffic() {
    let selected = [Some(10809), Some(10808)];
    let owned = HashSet::from([90, 80, 500]);
    let declared = ports(selected[0], selected[1]);
    let http = Socket {
        inode: 90,
        port: 10809,
        local: true,
        listen: true,
        udp: false,
        active: false,
    };
    let socks = Socket {
        inode: 80,
        port: 10808,
        ..http
    };
    let inspect = |rows: &[Socket]| check_sockets(&owned, rows, &declared, selected);
    assert_eq!(inspect(&[http, socks]), Some(false));
    assert_eq!(inspect(&[http]), None);
    for invalid in [
        Socket {
            inode: 777,
            ..socks
        },
        Socket {
            local: false,
            ..socks
        },
        Socket {
            listen: false,
            udp: true,
            ..socks
        },
    ] {
        assert_eq!(inspect(&[http, invalid]), None);
    }
    for extra in [
        Socket {
            port: 8000,
            inode: 500,
            ..http
        }, // additional owned TCP listener
        Socket {
            port: 8000,
            inode: 500,
            udp: true,
            listen: false,
            ..http
        },
        Socket { inode: 700, ..http }, // foreign listener sharing the port
        Socket {
            inode: 700,
            udp: true,
            listen: false,
            ..http
        },
    ] {
        assert_eq!(inspect(&[http, socks, extra]), None);
    }
    for active in [
        Socket {
            port: 58000,
            inode: 500,
            listen: false,
            active: true,
            ..http
        }, // outgoing
        Socket {
            inode: 500,
            listen: false,
            active: true,
            ..http
        }, // accepted
        Socket {
            inode: 0,
            listen: false,
            active: true,
            ..http
        }, // not yet accepted
    ] {
        assert_eq!(inspect(&[http, socks, active]), Some(true));
    }
    let time_wait = Socket {
        inode: 0,
        listen: false,
        active: false,
        ..http
    };
    assert_eq!(inspect(&[http, socks, time_wait]), Some(false));
}

#[test]
fn reads_explicit_json_fragments_and_refuses_ambiguous_disk_state() -> std::io::Result<()> {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir()?;
    let path = fs::canonicalize(temp.path())?;
    let process = path.join("process");
    let fragments = path.join("fragments");
    fs::create_dir(&process)?;
    fs::create_dir(&fragments)?;
    symlink("/", process.join("root"))?;
    fs::write(
        process.join("cmdline"),
        format!("xray\0run\0-d\0{}\0", fragments.display()),
    )?;
    fs::write(process.join("environ"), b"")?;
    let http = fragments.join("00-http.json");
    fs::write(
        &http,
        br#"{"inbounds":[{"listen":"127.0.0.1","port":10808,"protocol":"http"}]}"#,
    )?;
    fs::write(
        fragments.join("10-route.json"),
        br#"{"outbounds":[{"protocol":"freedom"}],"routing":{"rules":[]}}"#,
    )?;
    assert_eq!(
        configuration(&process, u64::MAX).and_then(|p| p.http),
        Some(10808)
    );
    assert!(configuration(&process, 0).is_none());
    fs::write(
        process.join("environ"),
        b"XRAY_LOCATION_CONFDIR=/var/lib/x\0",
    )?;
    assert!(configuration(&process, u64::MAX).is_none());
    fs::write(process.join("environ"), b"")?;
    symlink(&http, fragments.join("20-alias.json"))?;
    assert!(configuration(&process, u64::MAX).is_none());
    fs::remove_file(fragments.join("20-alias.json"))?;
    fs::write(fragments.join("20-policy.yaml"), "api: {}")?;
    assert!(configuration(&process, u64::MAX).is_none());
    Ok(())
}

#[test]
fn manual_stop_requires_known_empty_stop_dependencies() {
    let properties = "RefuseManualStop=no\nBoundBy=\nRequiredBy=\nConsistsOf=\nPropagatesStopTo=\nTriggeredBy=\nWantedBy=multi-user.target\nExecStop=/usr/bin/true";
    let mut fields: HashMap<_, _> = properties
        .lines()
        .map(|line| line.split_once('=').unwrap())
        .collect();
    assert!(independently_stoppable(&fields));
    for key in [
        "BoundBy",
        "RequiredBy",
        "ConsistsOf",
        "PropagatesStopTo",
        "TriggeredBy",
    ] {
        fields.insert(key, "other.service");
        assert!(!independently_stoppable(&fields), "{key}");
        fields.remove(key);
        assert!(!independently_stoppable(&fields), "{key}");
        fields.insert(key, "");
    }
    fields.insert("RefuseManualStop", "yes");
    assert!(!independently_stoppable(&fields));
}
