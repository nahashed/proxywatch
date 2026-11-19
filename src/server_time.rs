use crate::i18n::Language;
use std::{env, ffi::OsStr};
use time::{OffsetDateTime, UtcOffset};
use tz::TimeZone;

pub(crate) fn event_times(lang: Language, seconds: u64) -> Option<(String, Option<String>)> {
    event_times_in(lang, seconds, env::var_os("TZ").as_deref())
}

fn event_times_in(
    lang: Language,
    seconds: u64,
    zone: Option<&OsStr>,
) -> Option<(String, Option<String>)> {
    let instant = OffsetDateTime::from_unix_timestamp(seconds.try_into().ok()?).ok()?;
    let utc = format!("{} UTC", calendar(lang, instant));
    let local = server_zone(zone).and_then(|zone| {
        let kind = zone.find_local_time_type(instant.unix_timestamp()).ok()?;
        let offset = UtcOffset::from_whole_seconds(kind.ut_offset()).ok()?;
        if offset.is_utc() {
            return None;
        }
        let date = calendar(lang, instant.checked_to_offset(offset)?);
        let name = kind.time_zone_designation();
        let offset = offset_label(offset);
        Some(if name.is_empty() {
            format!("{date} {offset}")
        } else {
            format!("{date} {name} ({offset})")
        })
    });
    Some((utc, local))
}

fn server_zone(zone: Option<&OsStr>) -> Option<TimeZone> {
    match zone {
        None => TimeZone::local().ok(),
        Some(zone) if zone.is_empty() => Some(TimeZone::utc()),
        Some(zone) => TimeZone::from_posix_tz(zone.to_str()?).ok(),
    }
}

fn calendar(lang: Language, date: OffsetDateTime) -> String {
    let (year, month, day) = date.to_calendar_date();
    let month = u8::from(month);
    let day = match lang {
        Language::En => format!("{year:04}-{month:02}-{day:02}"),
        Language::Ru => format!("{day:02}.{month:02}.{year:04}"),
    };
    format!("{day} {:02}:{:02}", date.hour(), date.minute())
}

fn offset_label(offset: UtcOffset) -> String {
    let (hours, minutes, seconds) = offset.as_hms();
    let sign = if offset.is_negative() { '-' } else { '+' };
    let label = format!("UTC{sign}{:02}:{:02}", hours.abs(), minutes.abs());
    if seconds == 0 {
        label
    } else {
        format!("{label}:{:02}", seconds.abs())
    }
}

#[cfg(test)]
#[path = "../tests/unit/server_time.rs"]
mod tests;
