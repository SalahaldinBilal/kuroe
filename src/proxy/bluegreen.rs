#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Starting,
    Switching,
    Draining,
    Done,
    RollingBack,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Healthy,
    Unhealthy(String),
    Timeout,
    Switched,
    SwitchFailed(String),
    Drained,
    RolledBack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    SwitchRoutes,
    Drain,
    StopOld,
    RestoreRoutes,
    StopNew,
    Report(String),
}

pub fn next(phase: Phase, event: Event) -> Result<(Phase, Vec<Step>), String> {
    use Event as E;
    use Phase as P;
    Ok(match (phase, event) {
        (P::Starting, E::Healthy) => (P::Switching, vec![Step::SwitchRoutes]),
        (P::Starting, E::Unhealthy(why)) => (P::RollingBack, vec![Step::StopNew, Step::Report(format!("new version is unhealthy: {why}"))]),
        (P::Starting, E::Timeout) => (P::RollingBack, vec![Step::StopNew, Step::Report("new version did not become healthy in time".into())]),
        (P::Switching, E::Switched) => (P::Draining, vec![Step::Drain]),
        (P::Switching, E::SwitchFailed(why)) => (
            P::RollingBack,
            vec![Step::RestoreRoutes, Step::StopNew, Step::Report(format!("switching traffic failed: {why}"))],
        ),
        (P::Draining, E::Drained) => (P::Done, vec![Step::StopOld]),
        (P::RollingBack, E::RolledBack) => (P::Failed, vec![]),
        (phase, event) => return Err(format!("{event:?} is not valid while {phase:?}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(events: Vec<Event>) -> (Phase, Vec<Step>) {
        let mut phase = Phase::Starting;
        let mut steps = Vec::new();
        for event in events {
            let (next_phase, mut new_steps) = next(phase, event).unwrap();
            phase = next_phase;
            steps.append(&mut new_steps);
        }
        (phase, steps)
    }

    #[test]
    fn healthy_rollout_switches_then_stops_old() {
        let (phase, steps) = run(vec![Event::Healthy, Event::Switched, Event::Drained]);
        assert_eq!(phase, Phase::Done);
        assert_eq!(steps, [Step::SwitchRoutes, Step::Drain, Step::StopOld]);
    }

    #[test]
    fn unhealthy_never_touches_routes() {
        let (phase, steps) = run(vec![Event::Unhealthy("port 80 refused".into()), Event::RolledBack]);
        assert_eq!(phase, Phase::Failed);
        assert_eq!(steps, [Step::StopNew, Step::Report("new version is unhealthy: port 80 refused".into())]);
    }

    #[test]
    fn timeout_rolls_back() {
        let (phase, steps) = run(vec![Event::Timeout, Event::RolledBack]);
        assert_eq!(phase, Phase::Failed);
        assert_eq!(steps[0], Step::StopNew);
    }

    #[test]
    fn failed_switch_restores_routes_before_stopping_new() {
        let (phase, steps) = run(vec![Event::Healthy, Event::SwitchFailed("traefik did not pick it up".into()), Event::RolledBack]);
        assert_eq!(phase, Phase::Failed);
        assert_eq!(&steps[..3], [Step::SwitchRoutes, Step::RestoreRoutes, Step::StopNew]);
    }

    #[test]
    fn old_is_never_stopped_before_drain() {
        assert!(next(Phase::Switching, Event::Drained).is_err());
        assert!(next(Phase::Starting, Event::Switched).is_err());
    }
}
