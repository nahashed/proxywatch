use crate::runtime_text::message;
use crate::{
    engine::Engine,
    notifications,
    relay::Relay,
    storage::{Event, NodeHealth, Paths, State},
    subscription::{self, Node, Subscription},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Serialize, Deserialize)]
struct Cache {
    source: String,
    #[serde(default)]
    subscription_hwid: crate::subscription_identity::HwidMode,
    data: Subscription,
}

impl Cache {
    fn matches(&self, source: &str, hwid: &crate::subscription_identity::HwidMode) -> bool {
        self.source == source && &self.subscription_hwid == hwid
    }
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn observe(health: &mut NodeHealth, latency: Option<u64>, threshold: u32, at: u64) {
    health.checked_at = at;
    health.latency_ms = latency;
    let available = if latency.is_some() {
        health.failures = 0;
        true
    } else {
        health.failures = health.failures.saturating_add(1);
        if health.available == Some(true) && health.failures < threshold {
            return;
        }
        false
    };
    if health.available != Some(available) {
        health.available = Some(available);
        health.since = at;
    }
}

fn record(
    state: &mut State,
    previous_health: &BTreeMap<String, Option<bool>>,
    selected: Option<&str>,
    previous_preference: Option<&str>,
    at: u64,
) {
    let previous = state.active.clone();
    let first_inventory = previous_health.is_empty();
    let route_event = match (state.outage, previous.as_deref(), selected) {
        (false, _, None) => Some("outage"),
        (true, _, Some(_)) => Some("recovery"),
        (_, Some(old), Some(new)) if old != new => {
            if previous_preference == Some(new) {
                Some("failback")
            } else {
                Some("switch")
            }
        }
        (false, _, Some(_)) if first_inventory => Some("snapshot"),
        _ => None,
    };
    state.outage = selected.is_none();
    state.active = selected.map(str::to_owned);
    let current = selected.and_then(|name| state.nodes.get(name));
    state.last_probe = Some(current.is_some_and(|health| health.latency_ms.is_some()));
    state.status = match (current, state.last_probe) {
        (None, _) => "outage",
        (_, Some(true)) => "healthy",
        _ => "degraded",
    }
    .into();
    let mut changes: Vec<_> = state
        .nodes
        .iter()
        .filter_map(|(name, health)| {
            let value = health.available?;
            (previous_health.get(name).copied().flatten() != Some(value))
                .then(|| (name.clone(), value))
        })
        .collect();
    let base = Event {
        at,
        from: previous.clone(),
        to: state.active.clone(),
        active: state.active.clone(),
        started_at: state.started_at,
        available: state
            .nodes
            .values()
            .filter(|health| health.available == Some(true))
            .count(),
        total: state.nodes.len(),
        ..Event::default()
    };
    let mut push = |kind: &str, changes| {
        let event = Event {
            kind: kind.into(),
            changes,
            ..base.clone()
        };
        state.pending.push(event.clone());
        state.history.push(event);
    };
    if let Some(kind) = route_event {
        let (related, independent) = changes.into_iter().partition(|(name, _)| {
            first_inventory
                || matches!(kind, "snapshot" | "outage" | "recovery")
                || Some(name.as_str()) == previous.as_deref()
                || Some(name.as_str()) == selected
        });
        push(kind, related);
        changes = independent;
    }
    for change in changes {
        push("nodes", vec![change]);
    }
}

struct Measurement<E> {
    engine: E,
    latency: u64,
}

struct Current<E> {
    node: Node,
    engine: E,
}

struct Round<'a> {
    nodes: &'a [Node],
    scan_reserves: bool,
    threshold: u32,
    stop: &'a AtomicBool,
}

impl Round<'_> {
    fn check<E>(
        &self,
        state: &mut State,
        active: &mut Option<Current<E>>,
        mut probe_active: impl FnMut(&E) -> Option<u64>,
        mut probe_new: impl FnMut(&Node) -> Option<Measurement<E>>,
        mut set_route: impl FnMut(Option<&E>) -> Result<()>,
        clock: impl Fn() -> Result<u64>,
    ) -> Result<bool> {
        let before = state
            .nodes
            .iter()
            .map(|(name, health)| (name.clone(), health.available))
            .collect();
        let previous_preference = state.preferred.clone();
        state
            .nodes
            .retain(|name, _| self.nodes.iter().any(|node| node.name == *name));
        for node in self.nodes {
            state.nodes.entry(node.name.clone()).or_default();
        }
        if state
            .preferred
            .as_ref()
            .is_some_and(|name| !state.nodes.contains_key(name))
        {
            state.preferred = None;
        }
        let mut checked = None;
        if let Some(current) = active {
            let keep = if self.nodes.contains(&current.node) {
                let health = state.nodes.entry(current.node.name.clone()).or_default();
                checked = Some(current.node.name.clone());
                observe(
                    health,
                    probe_active(&current.engine),
                    self.threshold,
                    clock()?,
                );
                health.available == Some(true)
            } else {
                false
            };
            if !keep {
                set_route(None)?;
                active.take();
            }
        }
        let needs_route = active.is_none();
        let full = needs_route || self.scan_reserves || state.preferred.is_none();
        let mut best: Option<(Current<E>, u64)> = None;
        let mut ordered: Vec<_> = self.nodes.iter().collect();
        ordered.sort_by_key(|node| {
            (
                state.preferred.as_ref() != Some(&node.name),
                state
                    .nodes
                    .get(&node.name)
                    .and_then(|health| health.latency_ms)
                    .unwrap_or(u64::MAX),
            )
        });
        for node in ordered {
            if self.stop.load(Ordering::Relaxed) {
                return Ok(false);
            }
            let health = state.nodes.entry(node.name.clone()).or_default();
            if checked.as_ref() == Some(&node.name)
                || !(full || health.available == Some(true) && health.failures > 0)
            {
                continue;
            }
            let measurement = probe_new(node);
            // When there is no usable route a failed candidate cannot count as a working reserve.
            let threshold = if needs_route { 1 } else { self.threshold };
            observe(
                health,
                measurement.as_ref().map(|m| m.latency),
                threshold,
                clock()?,
            );
            if let Some(measured) = measurement {
                if let Some(preferred) = &state.preferred {
                    // A known priority lets us restore traffic before the remaining probes finish.
                    if active.is_none() || preferred == &node.name {
                        set_route(Some(&measured.engine))?;
                        *active = Some(Current {
                            node: node.clone(),
                            engine: measured.engine,
                        });
                    }
                } else if best
                    .as_ref()
                    .is_none_or(|(_, latency)| measured.latency < *latency)
                {
                    best = Some((
                        Current {
                            node: node.clone(),
                            engine: measured.engine,
                        },
                        measured.latency,
                    ));
                }
            }
        }
        if self.stop.load(Ordering::Relaxed) {
            return Ok(false);
        }
        if state.preferred.is_none() {
            let existing = active.as_ref().and_then(|current| {
                Some((
                    &current.node.name,
                    state.nodes.get(&current.node.name)?.latency_ms?,
                ))
            });
            state.preferred = existing
                .into_iter()
                .chain(
                    best.as_ref()
                        .map(|(current, latency)| (&current.node.name, *latency)),
                )
                .min_by_key(|(_, latency)| *latency)
                .map(|(name, _)| name.clone());
        }
        if let Some((candidate, _)) = best
            && (active.is_none() || state.preferred.as_ref() == Some(&candidate.node.name))
        {
            set_route(Some(&candidate.engine))?;
            *active = Some(candidate);
        }
        record(
            state,
            &before,
            active.as_ref().map(|c| c.node.name.as_str()),
            previous_preference.as_deref(),
            clock()?,
        );
        Ok(full)
    }
}

fn latency(engine: &Engine, url: &str, timeout: Duration) -> Option<u64> {
    let start = Instant::now();
    engine.probe(url, timeout).ok()?;
    Some(u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX))
}

pub fn run(paths: &Paths) -> Result<()> {
    let _lock = paths.lock()?;
    let mut settings = paths.load_settings()?;
    crate::xray_binary::select_for_run(&mut settings.xray_binary);
    let timeout = Duration::from_secs(settings.timeout_seconds);
    let stop = Arc::new(AtomicBool::new(false));
    let (wake, sleep) = mpsc::sync_channel(1);
    let interrupted = Arc::clone(&stop);
    ctrlc::set_handler(move || {
        interrupted.store(true, Ordering::Relaxed);
        let _ = wake.try_send(());
    })
    .context(message!(install_signal_handler))?;
    let relay = Relay::open(settings.http_port, settings.socks_port)?;
    let mut state = paths.load_state()?;
    state.status = "starting".into();
    state.started_at = now()?;
    state.updated_at = state.started_at;
    paths.save_state(&mut state)?;
    let mut data = paths
        .read::<Cache>("subscription.json")?
        .filter(|cache| cache.matches(&settings.subscription_url, &settings.subscription_hwid))
        .map(|c| c.data);
    let mut refresh = Instant::now();
    let mut notify = Instant::now();
    let mut reserve_scan = Instant::now();
    let mut active: Option<Current<Engine>> = None;
    let mut previously_failed_refresh = false;
    while !stop.load(Ordering::Relaxed) {
        if Instant::now() >= refresh {
            let mut delay = settings.refresh_seconds;
            match subscription::fetch(&settings.subscription_url, &settings.subscription_hwid) {
                Ok(fresh) => {
                    if data.as_ref().is_none_or(|old| old.nodes != fresh.nodes) {
                        reserve_scan = Instant::now();
                    }
                    let cache = Cache {
                        source: settings.subscription_url.clone(),
                        subscription_hwid: settings.subscription_hwid.clone(),
                        data: fresh,
                    };
                    paths.write("subscription.json", &cache)?;
                    data = Some(cache.data);
                    previously_failed_refresh = false;
                }
                Err(error) => {
                    if !previously_failed_refresh {
                        eprintln!("{} {error}", message!(subscription_retry));
                    }
                    previously_failed_refresh = true;
                    delay = delay.min(settings.interval_seconds.max(5));
                }
            }
            refresh = Instant::now() + Duration::from_secs(delay);
        }
        let nodes = data.as_ref().map_or(&[][..], |d| d.nodes.as_slice());
        state.skipped = data.as_ref().map_or_else(Vec::new, |d| d.skipped.clone());
        let old_pending = state.pending.len();
        let full = Round {
            nodes,
            scan_reserves: Instant::now() >= reserve_scan,
            threshold: settings.failure_threshold,
            stop: &stop,
        }
        .check(
            &mut state,
            &mut active,
            |engine| latency(engine, &settings.check_url, timeout),
            |node| {
                let engine = Engine::start(&settings, node).ok()?;
                let latency = latency(&engine, &settings.check_url, timeout)?;
                Some(Measurement { engine, latency })
            },
            |engine| relay.set(engine.map(|engine| engine.route)),
            now,
        )?;
        if stop.load(Ordering::Relaxed) {
            state = paths.load_state()?;
            break;
        }
        if full {
            reserve_scan = Instant::now() + Duration::from_secs(settings.reserve_interval_seconds);
        }
        if state.pending.len() != old_pending {
            notify = Instant::now();
        }
        state.updated_at = now()?;
        state.diagnostics.clear();
        paths.save_state(&mut state)?;
        if Instant::now() >= notify {
            let mut index = 0;
            while index < state.pending.len() && !stop.load(Ordering::Relaxed) {
                let event = &state.pending[index];
                let proxy = active
                    .as_ref()
                    .filter(|_| state.last_probe == Some(true))
                    .map(|a| &a.engine.client);
                if notifications::deliver(&settings, event, proxy).is_err() {
                    eprintln!("{}", message!(delivery_retry));
                    notify = Instant::now() + Duration::from_secs(300);
                    index += 1;
                    continue;
                }
                state.pending.remove(index);
                paths.save_state(&mut state)?;
            }
        }
        if let Some(current) = &active {
            for url in &settings.diagnostic_urls {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let passed = current.engine.probe(url, timeout).is_ok();
                state.diagnostics.insert(url.clone(), passed);
            }
            if !settings.diagnostic_urls.is_empty() {
                paths.save_state(&mut state)?;
            }
        }
        if !stop.load(Ordering::Relaxed) {
            let until_check = Duration::from_secs(settings.interval_seconds);
            let until_reserves = reserve_scan.saturating_duration_since(Instant::now());
            match sleep.recv_timeout(until_check.min(until_reserves)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }
    relay.set(None)?;
    drop(active);
    state.status = "stopped".into();
    state.updated_at = now()?;
    paths.save_state(&mut state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_subscription_requires_the_same_url_and_hwid_mode() -> Result<()> {
        let source = "https://example.invalid/sub";
        let mut cache: Cache = serde_json::from_value(serde_json::json!({
            "source": source, "data": {"nodes": [], "skipped": []}
        }))?;
        let disabled = crate::subscription_identity::HwidMode::Toggle(false);
        let automatic = crate::subscription_identity::HwidMode::Toggle(true);
        let custom = crate::subscription_identity::HwidMode::Custom("cluster-node".into());
        assert!(cache.matches(source, &disabled));
        assert!(!cache.matches(source, &automatic));
        cache.subscription_hwid = automatic.clone();
        assert!(cache.matches(source, &automatic));
        assert!(!cache.matches(source, &disabled));
        assert!(!cache.matches(source, &custom));
        assert!(!cache.matches("https://example.invalid/other", &automatic));
        Ok(())
    }

    #[derive(Default)]
    struct Scenario {
        state: State,
        active: Option<Current<String>>,
        routes: Vec<Option<String>>,
        probes: Vec<String>,
        tick: u64,
    }

    impl Scenario {
        fn check(&mut self, nodes: &[(&str, Option<u64>)], full: bool) {
            self.tick += 1;
            let inventory: Vec<_> = nodes
                .iter()
                .map(|(name, _)| Node {
                    name: (*name).into(),
                    outbound: serde_json::Value::Null,
                })
                .collect();
            let measure = |name: &str| nodes.iter().find(|(n, _)| *n == name).unwrap().1;
            self.probes.clear();
            Round {
                nodes: &inventory,
                scan_reserves: full,
                threshold: 2,
                stop: &AtomicBool::new(false),
            }
            .check(
                &mut self.state,
                &mut self.active,
                |name| measure(name),
                |node| {
                    self.probes.push(node.name.clone());
                    measure(&node.name).map(|latency| Measurement {
                        engine: node.name.clone(),
                        latency,
                    })
                },
                |route| {
                    self.routes.push(route.cloned());
                    Ok(())
                },
                || Ok(self.tick),
            )
            .unwrap();
        }
    }

    #[test]
    fn fastest_is_sticky_and_failback_does_not_require_reserve_failure() {
        let mut scenario = Scenario::default();
        let healthy = [
            ("standby", Some(50)),
            ("primary", Some(12)),
            ("backup", Some(24)),
        ];
        scenario.check(&healthy, true);
        assert_eq!(scenario.state.preferred.as_deref(), Some("primary"));
        assert_eq!(scenario.state.history[0].kind, "snapshot");
        assert_eq!(scenario.state.history[0].changes.len(), 3);
        let changed_rtt = [
            ("standby", Some(14)),
            ("primary", Some(38)),
            ("backup", Some(12)),
        ];
        scenario.check(&changed_rtt, true);
        assert_eq!(scenario.state.active.as_deref(), Some("primary"));

        let failed = [
            ("standby", Some(20)),
            ("primary", None),
            ("backup", Some(12)),
        ];
        scenario.check(&failed, false);
        assert_eq!(scenario.state.active.as_deref(), Some("primary"));
        assert_eq!(scenario.state.last_probe, Some(false));
        assert!(scenario.probes.is_empty());
        scenario.check(&failed, false);
        assert_eq!(scenario.state.active.as_deref(), Some("backup"));
        assert_eq!(scenario.probes, ["backup", "standby"]);
        let event = scenario.state.history.last().unwrap();
        assert_eq!(event.kind, "switch");
        assert_eq!(event.changes, [("primary".into(), false)]);
        assert_eq!((event.available, event.total), (2, 3));
        scenario.check(&healthy, false);
        assert_eq!(scenario.state.active.as_deref(), Some("backup"));
        scenario.check(&healthy, true);
        assert_eq!(scenario.state.active.as_deref(), Some("primary"));
        let event = scenario.state.history.last().unwrap();
        assert_eq!(event.kind, "failback");
        assert_eq!(event.changes, [("primary".into(), true)]);
        scenario.check(&healthy, true);
        assert_eq!(scenario.state.history.len(), 3);
    }

    #[test]
    fn standby_changes_and_total_outage_are_distinct_and_do_not_repeat_after_restart() {
        let mut scenario = Scenario::default();
        scenario.check(&[("A", Some(1)), ("B", Some(2))], true);
        let lost_b = [("A", Some(4)), ("B", None)];
        scenario.check(&lost_b, true);
        scenario.check(&lost_b, false);
        assert_eq!(
            scenario.state.history.last().unwrap().changes,
            [("B".into(), false)]
        );
        scenario.check(&lost_b, true);
        scenario.check(&[("A", Some(3)), ("B", Some(1))], true);
        assert_eq!(
            scenario.state.history.last().unwrap().changes,
            [("B".into(), true)]
        );
        assert_eq!(scenario.state.active.as_deref(), Some("A"));
        let offline = [("A", None), ("B", None)];
        scenario.check(&offline, false);
        scenario.check(&offline, false);
        assert_eq!(scenario.state.history.last().unwrap().available, 0);
        assert_eq!(scenario.state.history.last().unwrap().changes.len(), 2);
        scenario.check(&offline, true);
        scenario.check(&[("A", None), ("B", Some(2))], false);
        scenario.active = None;
        scenario.check(&[("A", None), ("B", Some(2))], true);
        assert_eq!(
            scenario
                .state
                .history
                .iter()
                .map(|event| event.kind.as_str())
                .collect::<Vec<_>>(),
            ["snapshot", "nodes", "nodes", "outage", "recovery"]
        );
        assert_eq!(scenario.state.history.len(), scenario.state.pending.len());
        assert_eq!(scenario.state.preferred.as_deref(), Some("A"));
    }

    #[test]
    fn migrating_an_active_installation_reports_the_first_inventory_once() {
        for (old, kind) in [("A", "snapshot"), ("B", "switch")] {
            let mut scenario = Scenario::default();
            scenario.state.active = Some(old.into());
            let nodes = [("A", Some(1)), ("B", Some(2)), ("C", None)];
            scenario.check(&nodes, true);
            scenario.check(&nodes, true);
            assert_eq!(scenario.state.pending.len(), 1);
            assert_eq!(scenario.state.pending[0].kind, kind);
            assert_eq!(scenario.state.pending[0].changes.len(), 3);
        }
    }

    #[test]
    fn removed_primary_is_replaced_by_fastest_remaining_not_a_failback() {
        let mut scenario = Scenario::default();
        scenario.check(&[("A", Some(1)), ("B", Some(10)), ("C", Some(5))], true);
        scenario.check(&[("B", Some(10)), ("C", Some(5))], true);
        assert_eq!(scenario.state.preferred.as_deref(), Some("C"));
        assert_eq!(scenario.state.history.last().unwrap().kind, "switch");
        assert!(!scenario.state.nodes.contains_key("A"));
    }
}
