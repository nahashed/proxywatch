use super::*;
use crate::storage::Settings;

#[test]
fn device_header_is_stable_and_scoped_to_the_https_origin() -> Result<()> {
    let client = Client::new();
    let id = "0123456789abcdef0123456789abcdef";
    let header = |url, id: &str| -> Result<HeaderValue> {
        let request = prepare(&client, Url::parse(url)?, true, || Ok(id.to_owned()))?.build()?;
        Ok(request.headers()["x-hwid"].clone())
    };
    let first = header("https://example.invalid/sub/token", id)?;
    assert!(first.is_sensitive());
    assert_eq!(first, header("https://EXAMPLE.invalid:443/other?id=1", id)?);
    assert_ne!(first, header("https://other.invalid/sub", id)?);
    assert_ne!(first, header("https://example.invalid:444/sub", id)?);
    assert_ne!(
        first,
        header(
            "https://example.invalid/sub",
            "1123456789abcdef0123456789abcdef"
        )?
    );
    // Future releases must present the same identifier to an existing provider.
    assert_eq!(
        first.to_str()?,
        "0ee30765a8221f4c655b871fa2baffc29c7e424769329b98b27e288b6d60b566"
    );
    assert!(!first.to_str()?.contains(id));
    Ok(())
}

#[test]
fn custom_hwid_is_sent_without_transformation() -> Result<()> {
    let client = Client::new();
    let mode = crate::subscription_identity::HwidMode::Custom("my-x-hwid-01".into());
    let request = request(
        &client,
        Url::parse("https://example.invalid/subscription")?,
        &mode,
    )?
    .build()?;
    let header = &request.headers()["x-hwid"];
    assert_eq!(header, "my-x-hwid-01");
    assert!(header.is_sensitive());
    Ok(())
}

#[test]
fn custom_identifiers_are_checked_before_building_the_request() -> Result<()> {
    let client = Client::new();
    let https = Url::parse("https://example.invalid/sub")?;
    for value in [
        "a".repeat(9),
        "b".repeat(65),
        "invalid id-01".into(),
        "invalid\r\nid".into(),
        "данные-1234".into(),
        "invalid_id-01".into(),
    ] {
        let mode = HwidMode::Custom(value.clone());
        let error = request(&client, https.clone(), &mode).unwrap_err();
        assert!(!error.to_string().contains(&value));
    }
    for value in ["azAZ09=-01".to_owned(), "a".repeat(64)] {
        let mode = HwidMode::Custom(value);
        assert!(request(&client, https.clone(), &mode).is_ok());
        assert!(request(&client, Url::parse("http://example.invalid/sub")?, &mode).is_err());
    }
    Ok(())
}

#[test]
fn device_identification_is_opt_in_and_requires_https_and_valid_identity() -> Result<()> {
    let mut settings = Settings {
        subscription_url: "http://example.invalid/sub".into(),
        ..Settings::default()
    };
    assert!(!settings.subscription_hwid.enabled());
    settings.validate()?;
    let client = Client::new();
    let url = Url::parse(&settings.subscription_url)?;
    let no_identity = || panic!("unexpected access to the machine ID");
    let request = prepare(&client, url.clone(), false, no_identity)?.build()?;
    assert!(!request.headers().contains_key("x-hwid"));
    assert!(prepare(&client, url, true, no_identity).is_err());
    settings.subscription_hwid = crate::subscription_identity::HwidMode::Toggle(true);
    assert!(settings.validate().is_err());
    for id in ["uninitialized", "", "00000000000000000000000000000000"] {
        assert!(
            prepare(
                &client,
                Url::parse("https://example.invalid/")?,
                true,
                || Ok(id.to_owned())
            )
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn hwid_redirects_keep_the_original_https_origin() -> Result<()> {
    let source = Url::parse("https://example.invalid:443/a")?;
    let same = Url::parse("https://EXAMPLE.invalid/b")?;
    let changed_host = Url::parse("https://cdn.example.invalid/data")?;
    let changed_port = Url::parse("https://example.invalid:1443/data")?;
    let cleartext = Url::parse("http://example.invalid/a")?;
    let credentials = Url::parse("https://user@example.invalid/b")?;
    let previous = [source, same.clone()];
    assert!(allows_redirect(&same, &previous, true));
    for target in [&changed_host, &changed_port, &cleartext, &credentials] {
        assert!(!allows_redirect(target, &previous, true));
    }
    assert!(allows_redirect(&changed_host, &previous, false));
    assert!(!allows_redirect(&cleartext, &previous, false));
    Ok(())
}

#[test]
fn subscription_hwid_reaches_only_the_first_origin_over_tls() -> Result<()> {
    use std::{
        io::{BufRead, BufReader, ErrorKind, Write},
        net::TcpListener,
        sync::Arc,
        thread,
        time::Duration,
    };

    let source = TcpListener::bind(("127.0.0.1", 0))?;
    let destination = TcpListener::bind(("127.0.0.1", 0))?;
    let url = Url::parse(&format!("https://{}/sub", source.local_addr()?))?;
    let redirect = format!("https://{}/different", destination.local_addr()?);
    let identity = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()])?;
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![identity.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(identity.signing_key.serialize_der())
                .into(),
        )?;
    let timeout = Duration::from_secs(5);
    let client = Client::builder()
        .no_proxy()
        .timeout(timeout)
        .add_root_certificate(reqwest::Certificate::from_der(identity.cert.der())?)
        .redirect(redirect_policy(true))
        .build()?;
    let request = prepare(&client, url, true, || {
        Ok("1123456789abcdef0123456789abcdef".into())
    })?
    .build()?;
    let expected = request.headers()["x-hwid"].to_str()?.to_owned();

    let received = thread::scope(|scope| -> Result<_> {
        let server = scope.spawn(move || -> Result<_> {
            let (socket, _) = source.accept()?;
            socket.set_read_timeout(Some(timeout))?;
            socket.set_write_timeout(Some(timeout))?;
            let connection = rustls::ServerConnection::new(Arc::new(tls))?;
            let mut stream = rustls::StreamOwned::new(connection, socket);
            let mut hwid = None;
            for line in BufReader::new(&mut stream).lines() {
                let line = line?;
                if line.is_empty() { break; }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("x-hwid")
                {
                    hwid = Some(value.trim().to_owned());
                }
            }
            write!(stream, "HTTP/1.1 302 Found\r\nLocation: {redirect}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
            stream.flush()?;
            Ok(hwid)
        });
        let status = client.execute(request)?.status();
        assert_eq!(status, reqwest::StatusCode::FOUND);
        server.join().expect("TLS fixture panicked")
    })?;
    assert_eq!(received.as_deref(), Some(expected.as_str()));
    destination.set_nonblocking(true)?;
    assert_eq!(
        destination.accept().unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
    Ok(())
}

#[test]
fn custom_hwid_reaches_a_local_https_subscription_unchanged() -> Result<()> {
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::Arc,
        thread,
        time::Duration,
    };

    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let url = Url::parse(&format!("https://{}/subscription", listener.local_addr()?))?;
    let identity = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()])?;
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![identity.cert.der().clone()],
            rustls::pki_types::PrivatePkcs8KeyDer::from(identity.signing_key.serialize_der())
                .into(),
        )?;
    let timeout = Duration::from_secs(5);
    let client = Client::builder()
        .no_proxy()
        .timeout(timeout)
        .add_root_certificate(reqwest::Certificate::from_der(identity.cert.der())?)
        .redirect(redirect_policy(true))
        .build()?;
    let mode = crate::subscription_identity::HwidMode::Custom("my-x-hwid-01".into());

    let received = thread::scope(|scope| -> Result<_> {
        let server = scope.spawn(move || -> Result<_> {
            let (socket, _) = listener.accept()?;
            let connection = rustls::ServerConnection::new(Arc::new(tls))?;
            let mut stream = rustls::StreamOwned::new(connection, socket);
            let mut hwid = None;
            for line in BufReader::new(&mut stream).lines() {
                let line = line?;
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("x-hwid")
                {
                    hwid = Some(value.trim().to_owned());
                }
            }
            let body = b"proxies: []\n";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            stream.write_all(body)?;
            stream.flush()?;
            Ok(hwid)
        });
        let response = request(&client, url, &mode)?.send()?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(
            crate::subscription::parse(&response.bytes()?)?
                .nodes
                .is_empty()
        );
        server.join().expect("TLS fixture panicked")
    })?;
    assert_eq!(received.as_deref(), Some("my-x-hwid-01"));
    Ok(())
}
