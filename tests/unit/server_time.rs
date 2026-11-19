use super::*;

#[test]
fn moscow_zoneinfo_uses_the_offset_at_the_event() {
    let zone = Some(OsStr::new(":Europe/Moscow"));
    assert_eq!(
        event_times_in(Language::Ru, 1_795_100_520, zone),
        Some((
            "19.11.2026 15:02 UTC".into(),
            Some("19.11.2026 18:02 MSK (UTC+03:00)".into())
        ))
    );
    // Moscow repeated the hour from 01:00 to 01:59 on 26 October 2014.
    for (seconds, offset) in [(1_414_272_600, "+04:00"), (1_414_276_200, "+03:00")] {
        let (_, local) = event_times_in(Language::En, seconds, zone).unwrap();
        assert_eq!(local, Some(format!("2014-10-26 01:30 MSK (UTC{offset})")));
    }
}

#[test]
fn local_calendar_crosses_midnight_and_skips_the_dst_gap() {
    let zone = Some(OsStr::new("EST5EDT,M3.2.0,M11.1.0"));
    for (seconds, expected) in [
        (1_772_953_140, "2026-03-08 01:59 EST (UTC-05:00)"),
        (1_772_953_200, "2026-03-08 03:00 EDT (UTC-04:00)"),
        (1_795_047_300, "2026-11-18 19:15 EST (UTC-05:00)"),
    ] {
        let (_, local) = event_times_in(Language::En, seconds, zone).unwrap();
        assert_eq!(local.as_deref(), Some(expected));
    }
}

#[test]
fn utc_survives_an_unknown_zone_and_is_never_duplicated() {
    for zone in ["", "UTC0", ":Missing/Zone"] {
        assert_eq!(
            event_times_in(Language::En, 0, Some(OsStr::new(zone))),
            Some(("1970-01-01 00:00 UTC".into(), None))
        );
    }
    assert_eq!(event_times(Language::Ru, u64::MAX), None);
}
