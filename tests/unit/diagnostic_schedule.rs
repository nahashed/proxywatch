use super::*;
use serde_json::{Value, json};

fn node(name: &str, outbound: Value) -> Node {
    Node {
        name: name.into(),
        outbound,
    }
}

#[test]
fn fast_and_slow_diagnostics_have_independent_deadlines_from_the_primary() -> Result<()> {
    let resources: Vec<DiagnosticResource> = serde_json::from_value(json!([
        {"url":"fast","interval_seconds":2}, "legacy", "legacy", {"url":"slow","interval_seconds":12}
    ]))?;
    let mut state = State::default();
    let start = Instant::now();
    let mut diagnostics = Diagnostics::restore(
        &resources,
        5,
        Some(node("A", Value::Null)),
        &mut state,
        start,
        100,
    );
    let mut probes = ProbeDeadlines {
        active: start,
        reserves: start + Duration::from_secs(90),
    };
    let mut rounds = Vec::new();
    let mut requests: BTreeMap<_, Vec<u64>> = BTreeMap::new();
    let mut tick = start;
    while tick <= start + Duration::from_secs(12) {
        let seconds = tick.duration_since(start).as_secs();
        if tick >= probes.next_round() {
            rounds.push(seconds);
            probes.active = tick + Duration::from_secs(5);
        }
        if let Some(resource) = diagnostics.due(tick) {
            requests.entry(resource.url).or_default().push(seconds);
            resource.complete(&mut state, resource.url != "slow", tick, 100 + seconds);
        }
        if seconds == 8 {
            assert_eq!(state.diagnostics.get("slow"), Some(&false));
            assert_eq!(state.diagnostic_checked_at.get("slow"), Some(&100));
        }
        tick = probes.next_round().min(diagnostics.next().unwrap());
    }
    assert_eq!(rounds, [0, 5, 10]);
    assert_eq!(requests["fast"], [0, 2, 4, 6, 8, 10, 12]);
    assert_eq!(requests["legacy"], [0, 5, 10]);
    assert_eq!(requests["slow"], [0, 12]);
    Ok(())
}

#[test]
fn restarting_reuses_only_matching_recent_results_and_handles_wall_clock_jumps() -> Result<()> {
    let resources: Vec<DiagnosticResource> =
        serde_json::from_value(json!(["recent", "future", "old", "legacy"]))?;
    let mut state: State = serde_json::from_value(json!({
        "diagnostics":{"recent":true,"future":false,"old":true,"legacy":false,"removed":true},
        "diagnostic_checked_at":{"recent":98,"future":200,"old":30,"removed":40}
    }))?;
    let start = Instant::now();
    let mut diagnostics = Diagnostics::restore(
        &resources,
        10,
        Some(node("A", Value::Null)),
        &mut state,
        start,
        100,
    );
    assert!(
        !state.diagnostics.contains_key("removed")
            && !state.diagnostic_checked_at.contains_key("removed")
    );
    let mut immediately_due = Vec::new();
    while let Some(resource) = diagnostics.due(start) {
        immediately_due.push(resource.url);
        resource.complete(&mut state, true, start, 90);
    }
    assert_eq!(immediately_due, ["future", "old", "legacy"]);
    assert_eq!(diagnostics.next(), Some(start + Duration::from_secs(8)));
    assert_eq!(state.diagnostic_checked_at["recent"], 98);
    let after = start + Duration::from_secs(8);
    let recent = diagnostics.due(after).unwrap();
    assert_eq!(recent.url, "recent");
    recent.complete(&mut state, false, after, u64::MAX);
    assert_eq!(diagnostics.next(), Some(start + Duration::from_secs(10)));
    Ok(())
}

#[test]
fn outage_recovery_and_replacement_of_a_node_with_the_same_name_invalidate_results() {
    let resources = [DiagnosticResource::Url("target".into())];
    let first = node("A", json!({"protocol":"first"}));
    let replaced = node("A", json!({"protocol":"second"}));
    let start = Instant::now();
    let mut state = State::default();
    let mut diagnostics =
        Diagnostics::restore(&resources, 120, Some(first.clone()), &mut state, start, 50);
    diagnostics
        .due(start)
        .unwrap()
        .complete(&mut state, true, start, 50);
    diagnostics.select_route(Some(&first), &mut state, start);
    assert!(diagnostics.due(start).is_none());
    assert_eq!(state.diagnostics.get("target"), Some(&true));
    diagnostics.select_route(Some(&replaced), &mut state, start);
    assert!(state.diagnostics.is_empty() && state.diagnostic_checked_at.is_empty());
    diagnostics
        .due(start)
        .unwrap()
        .complete(&mut state, false, start, 51);
    diagnostics.select_route(None, &mut state, start);
    assert!(state.diagnostics.is_empty() && state.diagnostic_checked_at.is_empty());
    assert!(diagnostics.next().is_none() && diagnostics.due(start).is_none());
    diagnostics.select_route(Some(&first), &mut state, start);
    assert_eq!(diagnostics.next(), Some(start));
}
