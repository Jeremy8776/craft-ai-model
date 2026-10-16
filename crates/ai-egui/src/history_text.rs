//! The words the history view shows. Pure functions, no egui, like [`crate::text`].

use craft_ai_core::history::report::{Totals, amount_text, time_text};
use craft_ai_core::{ErrorKind, HistoryEntry, Locality, Outcome};

/// A short phrase for why a job failed. Never the provider's own message.
pub fn failure_reason(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Unsupported => "not supported",
        ErrorKind::InvalidRequest => "bad request",
        ErrorKind::Auth => "sign-in problem",
        ErrorKind::RateLimited => "rate limited",
        ErrorKind::Quota => "out of credit",
        ErrorKind::Network => "network problem",
        ErrorKind::Provider => "provider error",
        ErrorKind::Cancelled => "cancelled",
        ErrorKind::ConsentRequired => "needed agreement",
        ErrorKind::Internal => "internal error",
    }
}

pub fn outcome_label(outcome: Option<&Outcome>) -> String {
    match outcome {
        Some(Outcome::Done) => "Done".to_owned(),
        Some(Outcome::Failed { kind }) => format!("Failed ({})", failure_reason(*kind)),
        Some(Outcome::Cancelled) => "Cancelled".to_owned(),
        None => "Not finished".to_owned(),
    }
}

fn seconds(ms: u64) -> String {
    if ms < 100_000 {
        format!("{}.{} s", ms / 1000, ms % 1000 / 100)
    } else {
        format!("{} s", ms / 1000)
    }
}

/// One job on one line.
pub fn entry_line(entry: &HistoryEntry, offset_minutes: i32) -> String {
    let s = &entry.start;
    let place = match s.locality {
        Locality::Local => "this computer",
        Locality::Remote => "remote",
    };
    let mut parts = vec![
        time_text(s.at_ms, offset_minutes),
        s.app.clone(),
        s.role_label.clone(),
        format!("{} on {} ({place})", s.model, s.connection_label),
        outcome_label(entry.outcome()),
    ];
    if let Some(ms) = entry.duration_ms() {
        parts.push(seconds(ms));
    }
    if let Some((micros, currency)) = entry.reported_cost() {
        parts.push(format!("{} {currency}", amount_text(micros)));
    }
    parts.join("  \u{b7}  ")
}

/// "12 jobs: 9 done, 2 failed, 1 cancelled."
pub fn counts_line(t: &Totals) -> String {
    let noun = if t.jobs == 1 { "job" } else { "jobs" };
    let mut bits = vec![format!("{} done", t.done)];
    if t.failed > 0 {
        bits.push(format!("{} failed", t.failed));
    }
    if t.cancelled > 0 {
        bits.push(format!("{} cancelled", t.cancelled));
    }
    if t.unfinished > 0 {
        bits.push(format!("{} not finished", t.unfinished));
    }
    format!("{} {noun}: {}.", t.jobs, bits.join(", "))
}

/// What the providers reported spending, per currency, and how many jobs reported nothing.
pub fn cost_line(t: &Totals) -> String {
    let mut text = if t.cost_micros.is_empty() {
        "No cost reported.".to_owned()
    } else {
        let parts: Vec<String> = t
            .cost_micros
            .iter()
            .map(|(currency, micros)| format!("{} {currency}", amount_text(*micros)))
            .collect();
        format!("Reported cost: {}.", parts.join(", "))
    };
    if t.without_cost > 0 && !t.cost_micros.is_empty() {
        let noun = if t.without_cost == 1 { "job" } else { "jobs" };
        text.push_str(&format!(" {} {noun} reported no cost.", t.without_cost));
    }
    text
}

/// A heading line for a day or month group.
pub fn group_line(label: &str, t: &Totals) -> String {
    let cost = if t.cost_micros.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = t
            .cost_micros
            .iter()
            .map(|(c, m)| format!("{} {c}", amount_text(*m)))
            .collect();
        format!("  \u{b7}  {}", parts.join(", "))
    };
    let noun = if t.jobs == 1 { "job" } else { "jobs" };
    format!("{label}  \u{b7}  {} {noun}{cost}", t.jobs)
}

pub fn skipped_line(n: usize) -> String {
    if n == 1 {
        "1 line in the history could not be read and was left out.".to_owned()
    } else {
        format!("{n} lines in the history could not be read and were left out.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_ai_core::history::report::totals;
    use craft_ai_core::history::{EndRecord, HISTORY_SCHEMA, HistoryRecord, StartRecord, merge};
    use craft_ai_core::job::Usage;

    fn entries(outcomes: &[(Option<Outcome>, Option<u64>)]) -> Vec<HistoryEntry> {
        let mut records = Vec::new();
        for (i, (outcome, cost)) in outcomes.iter().enumerate() {
            let run = format!("r{i}");
            records.push(HistoryRecord::Start(StartRecord {
                v: HISTORY_SCHEMA,
                run: run.clone(),
                at_ms: 1_760_000_000_000 + i as u64,
                app: "photocraft".into(),
                role_id: "fill".into(),
                role_label: "Generative Fill".into(),
                capability: "image.inpaint".into(),
                connection: "c".into(),
                connection_label: "Hosted".into(),
                model: "flux".into(),
                locality: Locality::Remote,
                destination: "d".into(),
                sent: Vec::new(),
                params: Vec::new(),
                provider_job: None,
                estimate: None,
                prompt: None,
            }));
            if let Some(o) = outcome {
                records.push(HistoryRecord::End(EndRecord {
                    v: HISTORY_SCHEMA,
                    run,
                    at_ms: 1_760_000_002_500 + i as u64,
                    outcome: o.clone(),
                    usage: cost.map(|c| Usage {
                        cost_micros: Some(c),
                        currency: Some("USD".into()),
                        ..Usage::default()
                    }),
                }));
            }
        }
        merge(records, 0).entries
    }

    #[test]
    fn a_job_reads_as_one_plain_line() {
        let e = entries(&[(Some(Outcome::Done), Some(31_000))]);
        assert_eq!(
            entry_line(&e[0], 0),
            "2025-10-09 08:53  \u{b7}  photocraft  \u{b7}  Generative Fill  \u{b7}  flux on Hosted (remote)  \u{b7}  Done  \u{b7}  2.5 s  \u{b7}  0.03 USD"
        );
    }

    #[test]
    fn failures_say_why_without_the_providers_words() {
        assert_eq!(
            outcome_label(Some(&Outcome::Failed {
                kind: ErrorKind::Quota
            })),
            "Failed (out of credit)"
        );
        assert_eq!(outcome_label(None), "Not finished");
    }

    #[test]
    fn counts_and_costs_are_honest_about_what_is_unknown() {
        let e = entries(&[
            (Some(Outcome::Done), Some(40_000)),
            (Some(Outcome::Done), None),
            (
                Some(Outcome::Failed {
                    kind: ErrorKind::Network,
                }),
                None,
            ),
            (None, None),
        ]);
        let t = totals(&e);
        assert_eq!(counts_line(&t), "4 jobs: 2 done, 1 failed, 1 not finished.");
        assert_eq!(
            cost_line(&t),
            "Reported cost: 0.04 USD. 1 job reported no cost."
        );
        assert_eq!(cost_line(&totals(&e[..0])), "No cost reported.");
        assert_eq!(counts_line(&totals(&e[..0])), "0 jobs: 0 done.");
    }

    #[test]
    fn skipped_lines_are_mentioned_in_the_singular_and_plural() {
        assert!(skipped_line(1).starts_with("1 line "));
        assert!(skipped_line(3).starts_with("3 lines "));
    }
}
