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
            .and_then(|inbound| Ports::default().add(inbound).ok())
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
fn legacy_timeout_is_an_http_only_unsigned_setting() {
    use serde_json::json;
    let input = |protocol, timeout| {
        serde_json::from_value::<Inbound>(json!({
            "listen":"127.0.0.1", "port":10809, "protocol":protocol, "settings":{"timeout":timeout}
        }))
    };
    let mut plain = Ports::default();
    plain.add(input("http", json!(0)).unwrap()).unwrap();
    assert_eq!(
        plain.check_http_timeout(|| panic!("no version query needed")),
        Ok(())
    );
    let mut configured = Ports::default();
    configured.add(input("http", json!(300)).unwrap()).unwrap();
    assert_eq!(
        configured.check_http_timeout(|| Some([1, 8, 24])),
        Err(Blocker::LegacyHttpTimeout)
    );
    assert_eq!(
        configured.check_http_timeout(|| Some([24, 9, 18])),
        Err(Blocker::LegacyHttpTimeout)
    );
    for version in [[24, 9, 19], [26, 3, 27], [26, 7, 28]] {
        assert_eq!(configured.check_http_timeout(|| Some(version)), Ok(()));
    }
    assert_eq!(
        configured.check_http_timeout(|| None),
        Err(Blocker::MissingTimeoutVersion)
    );
    for value in [json!(0), json!(300)] {
        assert_eq!(
            Ports::default().add(input("socks", value).unwrap()),
            Err(Blocker::InboundPolicy)
        );
    }
    for value in [
        json!(null),
        json!(true),
        json!("300"),
        json!(-1),
        json!(2.5),
        json!(4_294_967_296u64),
    ] {
        assert!(input("http", value).is_err());
    }
}

#[test]
fn the_running_version_requires_one_complete_xray_startup_banner() {
    let banner = "Xray 26.3.27 (Xray, Penetrates Everything.) build (go1.26 linux/amd64)\n";
    assert_eq!(banner_version(banner.as_bytes()), Some([26, 3, 27]));
    for bad in [
        "",
        "Xray 26.3.27",
        "Xray v26.3.27 (Xray, Penetrates Everything.)",
        "Xray 26.3.27 extra (Xray, Penetrates Everything.)",
        &banner.repeat(2),
    ] {
        assert_eq!(banner_version(bad.as_bytes()), None);
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
        address: Address::Loopback,
        listen: true,
        udp: false,
        active: false,
        incoming: false,
        closing: false,
    };
    let socks = Socket {
        inode: 80,
        port: 10808,
        ..http
    };
    let inspect = |rows: &[Socket]| check_sockets(&owned, rows, &declared, selected);
    assert_eq!(inspect(&[http, socks]), Ok(None));
    for one_port in [[selected[0], None], [None, selected[1]]] {
        assert_eq!(
            check_sockets(&owned, &[http, socks], &declared, one_port),
            Err(Blocker::SelectBothPorts)
        );
    }
    assert_eq!(inspect(&[http]), Err(Blocker::SocketMismatch));
    for invalid in [
        Socket {
            inode: 777,
            ..socks
        },
        Socket {
            address: Address::AnyV4,
            ..socks
        },
        Socket {
            listen: false,
            udp: true,
            ..socks
        },
    ] {
        assert!(inspect(&[http, invalid]).is_err());
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
        assert!(inspect(&[http, socks, extra]).is_err());
    }
    let flow = Socket {
        inode: 500,
        listen: false,
        active: true,
        incoming: true,
        ..http
    };
    assert_eq!(inspect(&[http, socks, flow]), Ok(Some(Blocker::Clients)));
    assert_eq!(
        inspect(&[http, socks, Socket { inode: 0, ..flow }]),
        Ok(Some(Blocker::Clients))
    );
    assert_eq!(
        inspect(&[
            http,
            socks,
            Socket {
                port: 58000,
                ..flow
            }
        ]),
        Ok(Some(Blocker::OtherTraffic))
    );
    assert_eq!(
        inspect(&[
            http,
            socks,
            Socket {
                incoming: false,
                closing: true,
                ..flow
            }
        ]),
        Ok(Some(Blocker::Closing))
    );
    let time_wait = Socket {
        inode: 0,
        listen: false,
        active: false,
        ..http
    };
    assert_eq!(inspect(&[http, socks, time_wait]), Ok(None));
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
        configuration(&process, u64::MAX).ok().and_then(|p| p.http),
        Some(10808)
    );
    assert_eq!(
        configuration(&process, 0).err(),
        Some(Blocker::ConfigChanged)
    );
    fs::write(
        process.join("environ"),
        b"XRAY_LOCATION_CONFDIR=/var/lib/x\0",
    )?;
    assert_eq!(
        configuration(&process, u64::MAX).err(),
        Some(Blocker::ConfigSource)
    );
    fs::write(process.join("environ"), b"")?;
    symlink(&http, fragments.join("20-alias.json"))?;
    assert_eq!(
        configuration(&process, u64::MAX).err(),
        Some(Blocker::ConfigIdentity)
    );
    fs::remove_file(fragments.join("20-alias.json"))?;
    fs::write(fragments.join("20-policy.yaml"), "api: {}")?;
    assert_eq!(
        configuration(&process, u64::MAX).err(),
        Some(Blocker::ConfigFormat)
    );
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

#[test]
fn socket_counts_separate_live_clients_from_closing_connections() {
    let listener = Socket {
        inode: 12,
        port: 10808,
        address: Address::Loopback,
        listen: true,
        udp: false,
        active: false,
        incoming: false,
        closing: false,
    };
    let established = Socket {
        inode: 13,
        listen: false,
        active: true,
        incoming: true,
        ..listener
    };
    let closing = Socket {
        inode: 14,
        incoming: false,
        closing: true,
        ..established
    };
    let time_wait = Socket {
        inode: 0,
        active: false,
        ..closing
    };
    let outgoing = Socket {
        inode: 15,
        port: 54321,
        ..established
    };
    let another_interface = Socket {
        address: Address::Elsewhere,
        ..established
    };
    let wildcard = Socket {
        address: Address::AnyV4,
        ..listener
    };
    let rows = [
        wildcard,
        established,
        closing,
        time_wait,
        outgoing,
        another_interface,
    ];
    let reports = port_reports([Some(10808), Some(10809)], Some(&rows));
    assert_eq!(
        (reports[0].listening, reports[0].clients, reports[0].closing),
        (Some(true), Some(1), Some(2))
    );
    assert_eq!(
        (reports[1].listening, reports[1].clients, reports[1].closing),
        (Some(false), Some(0), Some(0))
    );
    for (address, expected) in [(Address::Elsewhere, Some(false)), (Address::AnyV6, None)] {
        let listener = Socket {
            address,
            ..listener
        };
        let report = port_reports(
            [Some(listener.port), None],
            Some(&[listener, another_interface]),
        );
        assert_eq!(
            (report[0].listening, report[0].clients),
            (expected, Some(0))
        );
    }
    let unknown = port_reports([Some(10808), None], None);
    assert_eq!((unknown[0].listening, unknown[0].clients), (None, None));
}

#[test]
fn maps_a_delegated_unified_cgroup_through_the_container_mount_root() {
    let mountinfo = "38 27 0:31 /lxc/ct.ns /sys/fs/cgroup ro,nosuid,nodev - cgroup2 cgroup rw";
    let unit = Path::new("/lxc/ct.ns/system.slice/xray.service");
    let candidates = cgroup_directories(unit, mountinfo);
    assert_eq!(
        candidates.first().unwrap(),
        Path::new("/sys/fs/cgroup/system.slice/xray.service")
    );
    let unit = Path::new("/system.slice/xray.service");
    assert_eq!(
        cgroup_directories(unit, mountinfo),
        [PathBuf::from("/sys/fs/cgroup/system.slice/xray.service")]
    );
    assert!(cgroup_directories(unit, "38 27 0:31 / /sys/fs/cgroup rw - tmpfs tmpfs rw").is_empty());
}

#[test]
fn decodes_loopback_wildcards_and_unrelated_interface_addresses() {
    fn encoded(bytes: &[u8]) -> String {
        bytes
            .chunks_exact(4)
            .map(|word| format!("{:08X}", u32::from_ne_bytes(word.try_into().unwrap())))
            .collect()
    }
    for (v4, expected) in [
        (Ipv4Addr::LOCALHOST, Address::Loopback),
        (Ipv4Addr::UNSPECIFIED, Address::AnyV4),
        (Ipv4Addr::new(127, 0, 0, 2), Address::Elsewhere),
    ] {
        assert_eq!(Address::from_proc(&encoded(&v4.octets())), Some(expected));
    }
    for (v6, expected) in [
        (
            Ipv4Addr::LOCALHOST.to_ipv6_mapped(),
            Address::MappedLoopback,
        ),
        (Ipv6Addr::LOCALHOST, Address::Elsewhere),
        (Ipv6Addr::UNSPECIFIED, Address::AnyV6),
    ] {
        assert_eq!(Address::from_proc(&encoded(&v6.octets())), Some(expected));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn identifies_a_real_listening_descriptor_and_its_accept_side_only() -> std::io::Result<()> {
    use std::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let address = listener.local_addr()?;
    let _client = TcpStream::connect(address)?;
    let (_accepted, _) = listener.accept()?;
    let rows = sockets().unwrap();
    let mut report = port_reports([Some(address.port()), None], Some(&rows));
    listener_owners(&mut report, &rows);
    assert_eq!(report[0].clients, Some(1));
    // A separate IPv4 address can bind the same number without owning the loopback endpoint.
    let _other = TcpListener::bind((Ipv4Addr::new(127, 0, 0, 2), address.port()))?;
    assert!(owns_loopback_port(std::process::id(), address.port()));
    assert!(!owns_listeners(
        std::process::id(),
        Some(address.port()),
        None
    ));
    assert!(
        report[0]
            .owners
            .iter()
            .any(|owner| owner.pid == std::process::id())
    );
    Ok(())
}
