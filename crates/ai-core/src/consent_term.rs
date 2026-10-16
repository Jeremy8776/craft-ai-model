//! How long a person's yes lasts: the standard choices every app's consent prompt offers.
//!
//! The prompt is each app's own, but the choices and what they mean are shared, so "for an hour"
//! means the same in every app and a person is never asked about something they already agreed
//! to in another one's wording. The wording is the host's (it has its own language and look);
//! [`ConsentTerm::id`] is the stable name to store or translate against.

use serde::{Deserialize, Serialize};

use crate::consent::{ConsentLedger, Disclosure};

const HOUR: u64 = 60 * 60;
const DAY: u64 = 24 * HOUR;

/// How long a yes lasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentTerm {
    /// This job only. Nothing is remembered.
    ThisTime,
    /// For the next hour.
    OneHour,
    /// For the next day.
    OneDay,
    /// Until the person turns it off.
    UntilRevoked,
}

impl ConsentTerm {
    /// The choices in the order a prompt lists them, shortest first.
    pub const ALL: [ConsentTerm; 4] = [
        Self::ThisTime,
        Self::OneHour,
        Self::OneDay,
        Self::UntilRevoked,
    ];

    /// A stable name, for storing a default choice or looking up a translation.
    pub fn id(self) -> &'static str {
        match self {
            Self::ThisTime => "this_time",
            Self::OneHour => "one_hour",
            Self::OneDay => "one_day",
            Self::UntilRevoked => "until_revoked",
        }
    }

    /// Whether the yes is written to the consent file.
    pub fn remembers(self) -> bool {
        self != Self::ThisTime
    }

    /// When the yes lapses (unix seconds) if given at `now`; `None` if it does not.
    pub fn expires_at(self, now: u64) -> Option<u64> {
        match self {
            Self::ThisTime => Some(now),
            Self::OneHour => Some(now.saturating_add(HOUR)),
            Self::OneDay => Some(now.saturating_add(DAY)),
            Self::UntilRevoked => None,
        }
    }
}

impl ConsentLedger {
    /// Applies a person's choice and returns the ledger that covers the job they said yes to.
    /// Save `self` only if `term.remembers()`; start the job with the returned ledger either way.
    pub fn agree(&mut self, d: &Disclosure, term: ConsentTerm, now: u64) -> ConsentLedger {
        if !term.remembers() {
            let mut for_job = self.clone();
            for_job.grant(d);
            return for_job;
        }
        match term.expires_at(now) {
            Some(end) => self.grant_until(d, end),
            None => self.grant(d),
        }
        self.clone()
    }

    /// Drops agreements that have lapsed, so the file does not grow with old yeses.
    pub fn forget_lapsed(&mut self, now: u64) {
        self.records
            .retain(|r| r.expires_at.is_none_or(|end| now < end));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::CapabilityId;
    use crate::connection::fixtures::remote;
    use crate::ids::AppId;
    use crate::model::InputRole;
    use crate::payload::Payload;
    use crate::request::Request;

    fn disclosure() -> Disclosure {
        let request = Request::new(CapabilityId::parse("image.inpaint").unwrap()).with_input(
            InputRole::Source,
            Payload::inline("image/png", vec![0; 10]).unwrap(),
        );
        Disclosure::for_request(&AppId::new("photocraft").unwrap(), &remote("a"), &request)
    }

    #[test]
    fn the_choices_run_from_shortest_to_longest_with_distinct_ids() {
        let mut ids: Vec<_> = ConsentTerm::ALL.iter().map(|t| t.id()).collect();
        assert_eq!(ids, ["this_time", "one_hour", "one_day", "until_revoked"]);
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4);
        let now = 1_000;
        let ends: Vec<_> = ConsentTerm::ALL.iter().map(|t| t.expires_at(now)).collect();
        assert_eq!(
            ends,
            [Some(now), Some(now + 3_600), Some(now + 86_400), None]
        );
    }

    #[test]
    fn only_this_time_is_not_remembered() {
        let remembered: Vec<_> = ConsentTerm::ALL.iter().map(|t| t.remembers()).collect();
        assert_eq!(remembered, [false, true, true, true]);
    }

    #[test]
    fn this_time_covers_the_job_but_leaves_the_saved_ledger_asking() {
        let d = disclosure();
        let mut saved = ConsentLedger::default();
        let for_job = saved.agree(&d, ConsentTerm::ThisTime, 1_000);
        assert!(!for_job.needs_consent(&d, 1_000));
        assert!(saved.needs_consent(&d, 1_000), "nothing was remembered");
        assert!(saved.records.is_empty());
    }

    #[test]
    fn an_hour_is_remembered_until_the_hour_is_up() {
        let d = disclosure();
        let mut saved = ConsentLedger::default();
        let for_job = saved.agree(&d, ConsentTerm::OneHour, 1_000);
        assert!(!for_job.needs_consent(&d, 1_000));
        assert!(!saved.needs_consent(&d, 1_000 + 3_599));
        assert!(saved.needs_consent(&d, 1_000 + 3_600));
    }

    #[test]
    fn a_day_outlasts_an_hour_and_until_revoked_never_lapses() {
        let d = disclosure();
        let mut day = ConsentLedger::default();
        day.agree(&d, ConsentTerm::OneDay, 0);
        assert!(!day.needs_consent(&d, 86_399));
        assert!(day.needs_consent(&d, 86_400));
        let mut forever = ConsentLedger::default();
        forever.agree(&d, ConsentTerm::UntilRevoked, 0);
        assert!(!forever.needs_consent(&d, u64::MAX));
    }

    #[test]
    fn a_shorter_choice_does_not_shorten_a_longer_one_already_given() {
        let d = disclosure();
        let mut saved = ConsentLedger::default();
        saved.agree(&d, ConsentTerm::UntilRevoked, 0);
        saved.agree(&d, ConsentTerm::ThisTime, 10);
        assert!(!saved.needs_consent(&d, u64::MAX), "still open-ended");
    }

    #[test]
    fn forgetting_lapsed_agreements_keeps_the_live_ones() {
        let d = disclosure();
        let other = {
            let request = Request::new(CapabilityId::parse("image.upscale").unwrap()).with_input(
                InputRole::Source,
                Payload::inline("image/png", vec![0; 10]).unwrap(),
            );
            Disclosure::for_request(&AppId::new("photocraft").unwrap(), &remote("a"), &request)
        };
        let mut ledger = ConsentLedger::default();
        ledger.agree(&d, ConsentTerm::OneHour, 0);
        ledger.agree(&other, ConsentTerm::UntilRevoked, 0);
        ledger.forget_lapsed(3_600);
        assert_eq!(ledger.records.len(), 1);
        assert!(!ledger.needs_consent(&other, 3_600));
        assert!(ledger.needs_consent(&d, 3_600));
    }

    #[test]
    fn the_names_round_trip_through_json() {
        for t in ConsentTerm::ALL {
            let json = serde_json::to_string(&t).unwrap();
            assert_eq!(json, format!("\"{}\"", t.id()));
            assert_eq!(serde_json::from_str::<ConsentTerm>(&json).unwrap(), t);
        }
    }
}
