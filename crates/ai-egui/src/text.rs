//! The words the widgets show. Pure functions, no egui, so wording is unit-tested and an app that
//! draws its own UI can reuse the same sentences.
//!
//! Rules for the wording: say what leaves the machine in plain terms, never print contents, never
//! show a secret, and say money in the currency the provider reported without floating point
//! surprises.

use craft_ai_client::RoleStatus;
use craft_ai_core::consent::DisclosureItem;
use craft_ai_core::{Billing, CostEstimate, Disclosure, JobState, Locality, PayloadKind};

/// `1536` becomes `1.5 KB`. Sizes are decimal-free for bytes and one decimal above.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1000 {
        return format!("{n} bytes");
    }
    let mut value = n as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS.get(unit).copied().unwrap_or("TB"))
}

pub fn kind_label(kind: PayloadKind) -> &'static str {
    match kind {
        PayloadKind::Text => "text",
        PayloadKind::Image => "image",
        PayloadKind::Video => "video",
        PayloadKind::Audio => "audio",
        PayloadKind::Document => "document",
        PayloadKind::Other => "data",
    }
}

fn item_line(item: &DisclosureItem) -> String {
    let what = format!("{} ({})", kind_label(item.kind), item.role);
    match item.bytes {
        Some(n) => format!("{what}, {}", bytes(n)),
        None => what,
    }
}

/// One line per thing that would be sent, then one for the settings that ride along.
pub fn disclosure_lines(d: &Disclosure) -> Vec<String> {
    let mut lines: Vec<String> = d.items.iter().map(item_line).collect();
    if lines.is_empty() {
        lines.push("nothing but the request itself".to_owned());
    }
    if !d.param_names.is_empty() {
        lines.push(format!("settings: {}", d.param_names.join(", ")));
    }
    lines
}

/// The headline above the list.
pub fn consent_headline(d: &Disclosure, app_label: &str) -> String {
    format!("{app_label} will send the following to {}", d.destination)
}

pub fn locality_badge(l: Locality) -> &'static str {
    match l {
        Locality::Local => "Stays on this computer",
        Locality::Remote => "Leaves this computer",
    }
}

pub fn billing_label(b: Billing) -> &'static str {
    match b {
        Billing::Free => "free",
        Billing::Metered => "billed per use",
        Billing::SubscriptionAllowance => "uses your subscription allowance",
        Billing::Unknown => "billing unknown",
    }
}

/// `1_250_000` micros of `USD` becomes `about 1.25 USD`; sub-cent amounts keep four places.
pub fn cost_text(c: &CostEstimate) -> String {
    let whole = c.micros / 1_000_000;
    let frac = c.micros % 1_000_000;
    let amount = if c.micros < 10_000 {
        format!("{whole}.{:04}", frac / 100)
    } else {
        format!("{whole}.{:02}", frac / 10_000)
    };
    format!(
        "about {amount} {} ({})",
        c.currency,
        billing_label(c.billing)
    )
}

/// One sentence for a feature's state, safe to show.
pub fn status_line(label: &str, status: &RoleStatus) -> String {
    match status {
        RoleStatus::Ready(r) => format!(
            "{label}: {} on {} ({}, {})",
            r.model_label,
            r.connection_label,
            locality_badge(r.locality).to_lowercase(),
            billing_label(r.billing)
        ),
        RoleStatus::Unavailable { reason } => format!("{label} is off: {reason}"),
    }
}

/// How full a progress bar should be, or `None` when the provider reports no percentage.
pub fn progress_fraction(state: &JobState) -> Option<f32> {
    match state {
        JobState::Running {
            percent: Some(p), ..
        } => Some(f32::from((*p).min(100)) / 100.0),
        JobState::Succeeded => Some(1.0),
        _ => None,
    }
}

pub fn state_label(state: &JobState) -> String {
    match state {
        JobState::Queued => "Waiting to start".to_owned(),
        JobState::Running { percent: Some(p) } => format!("Working, {}%", (*p).min(100)),
        JobState::Running { percent: None } => "Working".to_owned(),
        JobState::Succeeded => "Done".to_owned(),
        JobState::Failed { error } => format!("Failed: {}", error.message),
        JobState::Cancelled => "Cancelled".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_ai_client::RoleReady;
    use craft_ai_core::{
        AiError, AppId, CapabilityId, ConnectionId, ErrorKind, InputRole, ModelId,
    };

    fn disclosure(items: Vec<DisclosureItem>, params: &[&str]) -> Disclosure {
        Disclosure {
            app: AppId::new("photocraft").unwrap(),
            connection: ConnectionId::new("c").unwrap(),
            capability: CapabilityId::parse("image.inpaint").unwrap(),
            locality: Locality::Remote,
            destination: "https://api.example.com".into(),
            items,
            param_names: params.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn byte_sizes_read_naturally() {
        assert_eq!(bytes(0), "0 bytes");
        assert_eq!(bytes(999), "999 bytes");
        assert_eq!(bytes(1_000), "1.0 KB");
        assert_eq!(bytes(1_536_000), "1.5 MB");
        assert_eq!(bytes(32 * 1000 * 1000 * 1000), "32.0 GB");
        assert_eq!(bytes(u64::MAX), "18446744.1 TB");
    }

    #[test]
    fn disclosure_says_what_and_how_big_without_contents() {
        let d = disclosure(
            vec![
                DisclosureItem {
                    role: InputRole::Source,
                    kind: PayloadKind::Image,
                    bytes: Some(2_500_000),
                },
                DisclosureItem {
                    role: InputRole::Prompt,
                    kind: PayloadKind::Text,
                    bytes: None,
                },
            ],
            &["size", "quality"],
        );
        assert_eq!(
            disclosure_lines(&d),
            [
                "image (source), 2.5 MB",
                "text (prompt)",
                "settings: size, quality"
            ]
        );
        assert!(consent_headline(&d, "PhotoCraft").contains("https://api.example.com"));
        assert_eq!(disclosure_lines(&disclosure(vec![], &[])).len(), 1);
    }

    #[test]
    fn money_is_formatted_from_millionths() {
        let c = |micros| CostEstimate {
            micros,
            currency: "USD".into(),
            billing: Billing::Metered,
        };
        assert_eq!(cost_text(&c(1_250_000)), "about 1.25 USD (billed per use)");
        assert_eq!(cost_text(&c(40_000)), "about 0.04 USD (billed per use)");
        assert_eq!(cost_text(&c(1_500)), "about 0.0015 USD (billed per use)");
        assert_eq!(cost_text(&c(0)), "about 0.0000 USD (billed per use)");
    }

    #[test]
    fn status_sentences() {
        let ready = RoleStatus::Ready(RoleReady {
            connection: ConnectionId::new("c").unwrap(),
            connection_label: "Ollama".into(),
            model: ModelId::new("llama3").unwrap(),
            model_label: "Llama 3".into(),
            locality: Locality::Local,
            billing: Billing::Free,
        });
        assert_eq!(
            status_line("Rewrite", &ready),
            "Rewrite: Llama 3 on Ollama (stays on this computer, free)"
        );
        let off = RoleStatus::Unavailable {
            reason: "no model is chosen for 'rewrite'".into(),
        };
        assert_eq!(
            status_line("Rewrite", &off),
            "Rewrite is off: no model is chosen for 'rewrite'"
        );
    }

    #[test]
    fn progress_is_clamped_and_optional() {
        assert_eq!(
            progress_fraction(&JobState::Running { percent: Some(40) }),
            Some(0.4)
        );
        assert_eq!(
            progress_fraction(&JobState::Running { percent: Some(250) }),
            Some(1.0)
        );
        assert_eq!(
            progress_fraction(&JobState::Running { percent: None }),
            None
        );
        assert_eq!(progress_fraction(&JobState::Queued), None);
        assert_eq!(progress_fraction(&JobState::Succeeded), Some(1.0));
    }

    #[test]
    fn state_labels() {
        assert_eq!(state_label(&JobState::Queued), "Waiting to start");
        assert_eq!(
            state_label(&JobState::Running { percent: Some(7) }),
            "Working, 7%"
        );
        assert_eq!(state_label(&JobState::Cancelled), "Cancelled");
        let failed = JobState::Failed {
            error: AiError::new(ErrorKind::Quota, "out of credits"),
        };
        assert_eq!(state_label(&failed), "Failed: out of credits");
    }
}
