use super::*;
use crate::storage::Telegram;

fn settings(mode: NotificationMode) -> Settings {
    Settings {
        telegram: Some(Telegram {
            token: "fixture".into(),
            chat_id: "fixture".into(),
        }),
        notification_mode: mode,
        ..Settings::default()
    }
}

#[test]
fn periodic_summaries_are_rate_limited_across_restarts_and_do_not_catch_up() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let paths = Paths::resolve(Some(temp.path().to_owned()))?;
    let mut state = paths.load_state()?;
    state.nodes.insert(
        "A".into(),
        NodeHealth {
            available: Some(true),
            ..Default::default()
        },
    );
    state.nodes.insert(
        "Z".into(),
        NodeHealth {
            available: Some(false),
            ..Default::default()
        },
    );
    let config = settings(NotificationMode::Summary);
    assert_eq!(config.notification_interval_seconds, 1800);
    let tick = Instant::now();
    let mut schedule = NotificationSchedule::restore(&config, &mut state, tick, 100);
    schedule.deliver_due(&paths, &mut state, tick, 100, |notice| {
        assert_eq!(notice.kind, "snapshot");
        assert_eq!(notice.changes, [("Z".into(), false), ("A".into(), true)]);
        // Even a crash between remote acknowledgement and clearing the queue leaves a deadline.
        let durable = paths.load_state()?;
        assert_eq!(durable.notification_attempted_at, Some(100));
        assert_eq!(durable.pending.len(), 1);
        Ok(())
    })?;
    assert!(paths.load_state()?.pending.is_empty());

    let tick = Instant::now();
    let mut restored = paths.load_state()?;
    let mut schedule = NotificationSchedule::restore(&config, &mut restored, tick, 900);
    assert_eq!(
        schedule.deadline(&restored),
        Some(tick + Duration::from_secs(1000))
    );
    schedule.deliver_due(&paths, &mut restored, tick, 900, |_| {
        panic!("early delivery")
    })?;
    let late = tick + Duration::from_secs(12_000);
    schedule.deliver_due(&paths, &mut restored, late, 12_900, |notice| {
        assert_eq!((notice.available, notice.total), (1, 2));
        Ok(())
    })?;
    schedule.deliver_due(&paths, &mut restored, late, 12_900, |_| {
        panic!("catch-up delivery")
    })?;
    assert!(restored.history.is_empty());
    Ok(())
}

#[test]
fn old_queue_and_failed_attempts_collapse_to_one_current_notification() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let paths = Paths::resolve(Some(temp.path().to_owned()))?;
    let mut state = paths.load_state()?;
    state.pending = vec![
        Event {
            kind: "outage".into(),
            ..Default::default()
        };
        250
    ];
    let config = settings(NotificationMode::Changes);
    let tick = Instant::now();
    let mut schedule = NotificationSchedule::restore(&config, &mut state, tick, 8000);
    assert_eq!(state.pending.len(), 1);
    schedule.deliver_due(&paths, &mut state, tick, 8000, |_| anyhow::bail!("offline"))?;
    let mut restored = paths.load_state()?;
    assert_eq!(restored.pending.len(), 1);
    restored.nodes.insert(
        "R".into(),
        NodeHealth {
            available: Some(true),
            latency_ms: Some(1),
            ..Default::default()
        },
    );
    restored.outage = true;
    record(&mut restored, &BTreeMap::new(), Some("R"), None, 8010);
    assert_eq!(restored.history.last().unwrap().kind, "recovery");
    assert_eq!(restored.pending.len(), 1);
    let mut schedule = NotificationSchedule::restore(&config, &mut restored, tick, 8010);
    schedule.deliver_due(&paths, &mut restored, tick, 8010, |_| {
        panic!("replayed on restart")
    })?;
    schedule.deliver_due(
        &paths,
        &mut restored,
        tick + Duration::from_secs(1790),
        9800,
        |event| {
            assert_eq!(event.active.as_deref(), Some("R"));
            assert_eq!((event.available, event.total), (1, 1));
            Ok(())
        },
    )?;
    assert!(restored.pending.is_empty());
    assert!(schedule.deadline(&restored).is_none());
    assert_eq!(paths.load_state()?.history.len(), 1);
    Ok(())
}

#[test]
fn one_round_preserves_route_and_reserve_changes_in_one_outgoing_message() {
    let before = BTreeMap::from([
        ("A".into(), Some(true)),
        ("B".into(), Some(true)),
        ("C".into(), Some(true)),
        ("D".into(), Some(true)),
    ]);
    let mut state = State {
        active: Some("A".into()),
        ..Default::default()
    };
    for (name, available) in [("A", false), ("B", true), ("C", false), ("D", false)] {
        state.nodes.insert(
            name.into(),
            NodeHealth {
                available: Some(available),
                ..Default::default()
            },
        );
    }
    record(&mut state, &before, Some("B"), Some("A"), 100);
    assert_eq!(state.pending.len(), 1);
    assert_eq!(state.pending[0].kind, "switch");
    assert_eq!(
        state.pending[0].changes,
        [
            ("A".into(), false),
            ("C".into(), false),
            ("D".into(), false)
        ]
    );
    assert_eq!(
        state
            .history
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        ["switch", "nodes"]
    );
    assert_eq!(
        state.history[1].changes,
        [("C".into(), false), ("D".into(), false)]
    );
}

#[test]
fn disabled_channels_do_not_schedule_attempts_or_keep_the_legacy_queue() {
    let mut state = State {
        pending: vec![Event::default(); 8],
        ..Default::default()
    };
    let schedule =
        NotificationSchedule::restore(&Settings::default(), &mut state, Instant::now(), 5);
    assert!(state.pending.is_empty());
    assert!(schedule.deadline(&state).is_none());
}
