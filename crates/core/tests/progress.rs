//! Ordered progress from the deployment engine's real transitions.
//!
//! The synthetic fixture in `recovery.rs` is a frozen acceptance witness and
//! must not be extended, so this target reuses it read-only and inspects the
//! events the streaming entry points report.

include!("recovery.rs");

fn commit(fixture: &mut Fixture, id: PlanId, events: &mut Vec<StepEvent>) -> OperationStatus {
    commit_deployment_with_progress(
        &mut fixture.journal,
        &mut fixture.effects,
        id,
        || false,
        |event| events.push(event),
    )
    .unwrap()
}

#[test]
fn apply_reports_each_durable_step_and_a_terminal_event_last() {
    let mut fixture = Fixture::new(WINNER);
    let id = fixture.plan();
    let mut events = Vec::new();
    let status = commit(&mut fixture, id, &mut events);

    assert_eq!(
        events.iter().map(|event| event.phase).collect::<Vec<_>>(),
        [
            Phase::Preparing,
            Phase::Applying,
            Phase::Applying,
            Phase::Completed
        ]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| (event.completed, event.total))
            .collect::<Vec<_>>(),
        [(0, 2), (1, 2), (2, 2), (2, 2)]
    );
    assert!(
        events.iter().all(|event| event.operation == status.id),
        "every event names the operation this call claimed"
    );
    // The last event is the terminal answer, and no step count is invented
    // beyond the number of durable steps.
    assert_eq!(events.last().unwrap().phase, Phase::Completed);
    assert_eq!(
        events.last().unwrap().completed,
        events.last().unwrap().total
    );
}

#[test]
fn restore_reports_each_durable_step_and_a_terminal_event_last() {
    let mut fixture = Fixture::new(WINNER);
    let id = fixture.plan();
    commit(&mut fixture, id, &mut Vec::new());

    let mut events = Vec::new();
    let status = restore_with_progress(
        &mut fixture.journal,
        &mut fixture.effects,
        &root(1, 1),
        &root(1, 2),
        |event| events.push(event),
    )
    .unwrap();

    assert_eq!(
        events.iter().map(|event| event.phase).collect::<Vec<_>>(),
        [
            Phase::Preparing,
            Phase::Restoring,
            Phase::Restoring,
            Phase::Completed
        ]
    );
    assert_eq!(
        events
            .iter()
            .map(|event| (event.completed, event.total))
            .collect::<Vec<_>>(),
        [(0, 2), (1, 2), (2, 2), (2, 2)]
    );
    assert!(events.iter().all(|event| event.operation == status.id));
    assert_eq!(events.last().unwrap().phase, Phase::Completed);
    assert_eq!(status.phase, Phase::Completed);
}

#[test]
fn a_replayed_claim_reports_only_its_terminal_answer() {
    let mut fixture = Fixture::new(WINNER);
    let id = fixture.plan();
    let first = commit(&mut fixture, id, &mut Vec::new());

    let mut replay = Vec::new();
    let second = commit(&mut fixture, id, &mut replay);

    assert_eq!(second.id, first.id);
    assert_eq!(second.phase, Phase::Completed);
    assert_eq!(
        replay,
        [StepEvent {
            operation: first.id,
            phase: Phase::Completed,
            completed: 0,
            total: 0,
        }]
    );
}
