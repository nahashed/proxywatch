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

fn visible_html(html: &str) -> String {
    html.replace("<b>", "")
        .replace("</b>", "")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[test]
fn both_channels_distinguish_node_changes_from_the_active_route() {
    let event = Event {
        at: 180,
        kind: "nodes".into(),
        active: Some("Primary".into()),
        available: 1,
        total: 2,
        started_at: 60,
        changes: vec![("Backup".into(), true)],
        ..Event::default()
    };
    for language in [Language::En, Language::Ru] {
        let expected = test_fixtures::messages(language);
        let settings = Settings {
            language,
            instance_name: "Test instance".into(),
            instance_description: "Monitoring".into(),
            ..Settings::default()
        };
        let alert = Alert::from_event(&settings, &event, false).unwrap();
        let plain = alert.text();
        let html = alert.html();
        assert_eq!(visible_html(&html), plain);
        for (message, prefix, bold) in [
            (plain.as_str(), "node_alert_plain", false),
            (html.as_str(), "node_alert_html", true),
        ] {
            let tail = message.strip_prefix(&expected[prefix]).unwrap();
            let blocks: Vec<_> = tail.strip_prefix("\n\n").unwrap().split("\n\n").collect();
            assert_eq!(blocks.len(), 2);
            for (block, key, utc_key, seconds) in [
                (blocks[0], "event_heading", "utc_event", event.at),
                (blocks[1], "start_heading", "utc_start", event.started_at),
            ] {
                let (heading, dates) = block.split_once('\n').unwrap();
                let label = &expected[key];
                if bold {
                    assert_eq!(heading, format!("<b>{label}</b>"));
                } else {
                    assert_eq!(heading, label);
                }
                let mut dates = dates.lines();
                assert_eq!(dates.next(), Some(expected[utc_key].as_str()));
                let (_, local) = crate::server_time::event_times(language, seconds).unwrap();
                if let Some(local) = local {
                    let label = &expected["server_label"];
                    let field = if bold {
                        format!("<b>{label}</b> {local}")
                    } else {
                        format!("{label} {local}")
                    };
                    assert_eq!(dates.next(), Some(field.as_str()));
                }
                assert_eq!(dates.next(), None);
            }
        }
    }
}

#[test]
fn omits_empty_purpose_and_redundant_or_unknown_start_time() {
    for language in [Language::En, Language::Ru] {
        let expected = test_fixtures::messages(language);
        for started_at in [0, 177, 179] {
            let event = Event {
                at: 179,
                started_at,
                kind: "snapshot".into(),
                ..Event::default()
            };
            let alert = Alert::from_event(
                &Settings {
                    language,
                    ..Settings::default()
                },
                &event,
                false,
            )
            .unwrap();
            let plain = alert.text();
            assert_eq!(visible_html(&alert.html()), plain);
            assert!(!plain.contains(&expected["purpose_label"]));
            assert!(!plain.contains(&expected["start_heading"]));
            assert_eq!(plain.matches(&expected["event_heading"]).count(), 1);
        }
    }
}

#[test]
fn escapes_user_data_only_in_html_and_redacts_urls_in_both_channels() {
    let settings = Settings {
        instance_name: "Test <b> & team".into(),
        instance_description: "Monitoring A & B".into(),
        ..Settings::default()
    };
    let event = Event {
        kind: "switch".into(),
        from: Some("https://example.invalid/private".into()),
        to: Some("Primary > A".into()),
        active: Some("Primary > A".into()),
        changes: vec![
            ("<i>Backup & reserve</i>".into(), true),
            ("https://example.invalid/private".into(), false),
        ],
        ..Event::default()
    };
    let alert = Alert::from_event(&settings, &event, false).unwrap();
    let plain = alert.text();
    let html = alert.html();
    assert_eq!(visible_html(&html), plain);
    for (raw, escaped) in [
        ("Test <b> & team", "Test &lt;b&gt; &amp; team"),
        ("Monitoring A & B", "Monitoring A &amp; B"),
        ("Primary > A", "Primary &gt; A"),
        (
            "<i>Backup & reserve</i>",
            "&lt;i&gt;Backup &amp; reserve&lt;/i&gt;",
        ),
    ] {
        assert!(plain.contains(raw));
        assert!(html.contains(escaped));
        assert!(!html.contains(raw));
    }
    for message in [plain, html] {
        assert!(!message.contains("example.invalid"));
        assert!(!message.contains("private"));
    }
}

#[test]
fn formats_each_event_in_both_languages_for_both_channels() {
    for language in [Language::En, Language::Ru] {
        let expected = test_fixtures::messages(language);
        let settings = Settings {
            language,
            ..Settings::default()
        };
        for kind in [
            "snapshot", "nodes", "switch", "failback", "outage", "recovery",
        ] {
            let event = Event {
                kind: kind.into(),
                active: Some("Primary".into()),
                available: 1,
                total: 2,
                ..Event::default()
            };
            let alert = Alert::from_event(&settings, &event, false).unwrap();
            let plain = alert.text();
            let html = alert.html();
            assert_eq!(alert.summary, expected[kind]);
            assert_eq!(plain.lines().next(), Some(expected[kind].as_str()));
            assert!(html.starts_with(&format!("<b>{}</b>\n\n", expected[kind])));
            assert_eq!(visible_html(&html), plain);
            let route = if kind == "outage" {
                "outage_route"
            } else {
                "normal_route"
            };
            assert!(plain.contains(&expected[route]));
        }
    }
}

#[test]
fn keeps_large_snapshots_within_telegrams_limit_after_html_parsing() {
    let event = Event {
        kind: "snapshot".into(),
        total: 500,
        available: 500,
        changes: (0..500).map(|_| ("<&𝄞>".repeat(25), true)).collect(),
        ..Event::default()
    };
    for language in [Language::En, Language::Ru] {
        let settings = Settings {
            language,
            ..Settings::default()
        };
        let alert = Alert::from_event(&settings, &event, false).unwrap();
        let visible = visible_html(&alert.html());
        assert_eq!(visible, alert.text());
        assert!(visible.encode_utf16().count() <= 4096);
        assert_eq!(
            visible.lines().filter(|line| line.starts_with('•')).count(),
            5
        );
        assert!(visible.contains(&test_fixtures::messages(language)["truncated"]));
    }
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
