use super::*;
use crate::test_fixtures;
use std::cell::RefCell;

#[test]
fn telegram_success_stops_and_failure_or_absent_route_uses_email() {
    for (enable_telegram, success, expected) in [
        (true, true, vec!["telegram"]),
        (true, false, vec!["telegram", "smtp"]),
        (false, false, vec!["smtp"]),
    ] {
        let calls = RefCell::new(Vec::new());
        route(
            enable_telegram.then_some(|| {
                calls.borrow_mut().push("telegram");
                ensure!(success, "test");
                Ok(())
            }),
            Some(|| {
                calls.borrow_mut().push("smtp");
                Ok(())
            }),
        )
        .unwrap();
        assert_eq!(*calls.borrow(), expected);
    }
}

#[test]
fn shapes_two_paragraphs_and_escapes_markdown_from_node_names() {
    let event = Event {
        at: 1,
        kind: "switch".into(),
        from: Some("One_[#1]".into()),
        to: Some("https://example.invalid/private".into()),
        active: Some("https://example.invalid/private".into()),
        available: 1,
        total: 2,
        started_at: 1,
        changes: vec![("One_[#1]".into(), false)],
    };
    let settings = Settings {
        instance_name: "Test instance".into(),
        ..Settings::default()
    };
    let alert = Alert::from_event(&settings, &event, false).unwrap();
    let text = alert.markdown();
    let expected = test_fixtures::messages(settings.language);
    assert!(text.starts_with(&format!("*Test instance*\n\n{}\n", expected["switch"])));
    assert!(text.contains(r"One\_\[\#1\]"));
    assert!(!text.contains("private"));
    assert!(text.contains(&format!("\n{}\n", expected["available"])));
    assert!(text.contains(&expected["started"]));
    assert_eq!(text.matches("\n\n").count(), 1);
    assert_eq!(escape_markdown(r"\*+#|"), r"\\\*\+\#\|");
}

#[test]
fn reserve_recovery_distinguishes_changed_node_from_traffic_route() {
    let settings = Settings {
        instance_name: "instance-a".into(),
        instance_description: "Monitoring".into(),
        ..Settings::default()
    };
    let event = Event {
        kind: "nodes".into(),
        active: Some("Primary".into()),
        available: 3,
        total: 4,
        changes: vec![("Backup".into(), true)],
        ..Event::default()
    };
    let alert = Alert::from_event(&settings, &event, false).unwrap();
    let expected = test_fixtures::messages(settings.language);
    assert!(alert.text.contains(&expected["node_change"]));
    assert!(alert.text.contains(&expected["purpose"]));
}

#[test]
fn formats_each_event_in_both_languages_for_both_channels() {
    for language in [Language::En, Language::Ru] {
        let expected = test_fixtures::messages(language);
        let settings = Settings {
            language,
            instance_name: "Home".into(),
            instance_description: "Monitoring".into(),
            ..Settings::default()
        };
        for kind in [
            "snapshot", "nodes", "switch", "failback", "outage", "recovery",
        ] {
            let event = Event {
                kind: kind.into(),
                changes: vec![("Backup".into(), true)],
                active: Some("Primary".into()),
                available: 2,
                total: 2,
                ..Event::default()
            };
            let alert = Alert::from_event(&settings, &event, false).unwrap();
            assert_eq!(alert.summary, expected[kind]);
            assert!(alert.text.contains(&expected["purpose"]));
            assert!(alert.markdown().contains(&expected["backup_up"]));
            assert!(alert.text.contains(&expected["both_available"]));
            let route = if kind == "outage" {
                "stopped"
            } else {
                "primary"
            };
            assert!(alert.text.contains(&expected[route]));
        }
    }
}

#[test]
fn keeps_snapshot_with_hundreds_of_nodes_within_one_telegram_message() {
    let event = Event {
        kind: "snapshot".into(),
        total: 500,
        available: 500,
        changes: (0..500).map(|_| ("🟢".repeat(100), true)).collect(),
        ..Event::default()
    };
    let alert = Alert::from_event(&Settings::default(), &event, false).unwrap();
    assert!(alert.markdown().encode_utf16().count() <= 4096);
    assert!(
        alert
            .text
            .contains(&test_fixtures::messages(Language::Ru)["truncated"])
    );
}

#[test]
fn delayed_outage_and_recovery_both_reach_telegram_when_the_proxy_returns() {
    let settings = Settings {
        telegram: Some(Telegram {
            token: "synthetic".into(),
            chat_id: "test".into(),
        }),
        ..Settings::default()
    };
    let mut event = Event {
        at: 1,
        kind: "outage".into(),
        from: Some("Primary".into()),
        to: None,
        ..Event::default()
    };
    let client = Client::builder().no_proxy().build().unwrap();
    let sent = RefCell::new(Vec::new());
    let send = |_: &Telegram, actual: &Client, alert: &Alert, _: Duration| {
        assert!(std::ptr::eq(actual, &client));
        sent.borrow_mut().push(alert.summary);
        Ok(())
    };
    let no_email = |_: &Smtp, _: &Alert, _: Duration| -> Result<()> { panic!("SMTP is disabled") };
    assert!(deliver_with(&settings, &event, None, send, no_email).is_err());
    assert!(sent.borrow().is_empty());
    deliver_with(&settings, &event, Some(&client), send, no_email).unwrap();
    event.kind = "recovery".into();
    event.to = Some("Reserve".into());
    deliver_with(&settings, &event, Some(&client), send, no_email).unwrap();
    let expected = test_fixtures::messages(settings.language);
    assert_eq!(
        *sent.borrow(),
        [expected["delayed"].as_str(), expected["recovery"].as_str()]
    );
}
