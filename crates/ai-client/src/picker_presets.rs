//! The presets at the top of the picker: the model each preset earns for a tool, and the one line
//! that says why. Pure functions over what `AiClient::picks` returns, so each rule has its own test.
//!
//! Only a model on a connection that is ready is offered: a preset must never lead to a model the
//! person cannot use. Nothing here binds anything; choosing is the picker's job.

use craft_ai_core::{Pick, PickedBy};

use crate::client::PickedChoice;

/// How a preset is named for people.
pub fn pick_label(pick: Pick) -> &'static str {
    match pick {
        Pick::Recommended => "Recommended",
        Pick::BestResult => "Best result",
        Pick::LowestCost => "Lowest cost",
        Pick::Fastest => "Fastest",
        Pick::Newest => "Newest",
        Pick::MostPopular => "Most popular",
    }
}

/// What a reported price is for. Images are priced one at a time and text by the million tokens.
/// A video price is for whatever one result is (a clip, a second), which the price field does not
/// say, so it is shown as "each" rather than guessed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PriceUnit {
    Image,
    Each,
    MillionTokens,
}

pub(crate) fn price_unit(capability: &str) -> PriceUnit {
    if capability.starts_with("video") {
        PriceUnit::Each
    } else if capability.starts_with("image") {
        PriceUnit::Image
    } else {
        PriceUnit::MillionTokens
    }
}

/// One line saying why this model earned the preset, from what is known and nothing more.
pub(crate) fn reason(picked: &PickedChoice, unit: PriceUnit) -> String {
    match &picked.by {
        PickedBy::Catalog => match picked.pick {
            Pick::Recommended => "Our pick: a balance of quality, speed and price.".to_owned(),
            Pick::BestResult => "Our pick for the best result.".to_owned(),
            Pick::Fastest => "Our pick for the fastest answer.".to_owned(),
            Pick::MostPopular => "Among the most used.".to_owned(),
            // Computed presets never come from the catalog.
            Pick::LowestCost | Pick::Newest => String::new(),
        },
        PickedBy::Free => "Free to run.".to_owned(),
        PickedBy::Price(micros) => {
            let unit = match unit {
                PriceUnit::Image => "per image",
                PriceUnit::Each => "each",
                PriceUnit::MillionTokens => "per million tokens",
            };
            format!("Lowest reported price: {} {unit}.", dollars(*micros))
        }
        PickedBy::Released(at) => format!("Released {}.", date(*at)),
    }
}

/// Millionths of a dollar as a price: two places from a cent up, four below that.
pub(crate) fn dollars(micros: u64) -> String {
    if micros >= 10_000 {
        let cents = (micros + 5_000) / 10_000;
        format!("${}.{:02}", cents / 100, cents % 100)
    } else {
        let hundredths = (micros + 50) / 100;
        format!("${}.{:04}", hundredths / 10_000, hundredths % 10_000)
    }
}

/// Unix seconds as `12 Mar 2026` (UTC).
pub(crate) fn date(seconds: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = i64::try_from(seconds / 86_400).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let name = MONTHS[usize::try_from(month - 1).unwrap_or(0).min(11)];
    format!("{day} {name} {year}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_price_names_only_the_unit_it_is_known_in() {
        use craft_ai_core::{ConnectionId, Locality, ModelId, Pick, PickedBy};

        use crate::client::Choice;
        let picked = PickedChoice {
            pick: Pick::LowestCost,
            by: PickedBy::Price(40_000),
            choice: Choice {
                connection: ConnectionId::new("c").unwrap(),
                connection_label: "C".into(),
                model: ModelId::new("m").unwrap(),
                model_label: "M".into(),
                locality: Locality::Remote,
            },
        };
        let said = |capability: &str| reason(&picked, price_unit(capability));
        assert_eq!(
            said("image.generate"),
            "Lowest reported price: $0.04 per image."
        );
        assert_eq!(said("video.generate"), "Lowest reported price: $0.04 each.");
        assert_eq!(
            said("text.complete"),
            "Lowest reported price: $0.04 per million tokens."
        );
    }

    #[test]
    fn prices_read_as_money() {
        assert_eq!(dollars(40_000), "$0.04");
        assert_eq!(dollars(2_500_000), "$2.50");
        assert_eq!(dollars(150), "$0.0002");
        assert_eq!(dollars(1_500), "$0.0015");
        assert_eq!(dollars(0), "$0.0000");
    }

    #[test]
    fn dates_read_as_a_day() {
        assert_eq!(date(0), "1 Jan 1970");
        assert_eq!(date(1_692_901_234), "24 Aug 2023");
        assert_eq!(date(1_709_164_800), "29 Feb 2024");
    }
}
