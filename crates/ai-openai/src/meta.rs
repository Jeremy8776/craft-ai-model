//! Price and date metadata from OpenRouter's `/models` list.
//!
//! OpenRouter reports, for every model, its price in US dollars per token as a string
//! (`"pricing":{"prompt":"0.00003","completion":"0.00006"}`) and `created`, the Unix time the
//! model was added to OpenRouter. The date is when OpenRouter listed it, which is a good proxy for
//! "newest" but not a release date, so the picker says "newest here". Prices for generated images
//! are not in this field (its `image` is the price of an image given as input), so image models get
//! no per-image price from here and the lowest-cost pick simply skips them.
//!
//! Only OpenRouter's own list is read this way: another server's `pricing` could mean anything.

use craft_ai_core::{ModelInfo, Pricing};
use serde_json::Value;

/// Dollars per token as a JSON string or number, to millionths of a dollar per million tokens.
fn micros_per_mtok(v: Option<&Value>) -> Option<u64> {
    let dollars = match v? {
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    // One dollar a token is already absurd; anything beyond is a unit mix-up, not a price.
    if !dollars.is_finite() || !(0.0..=1.0).contains(&dollars) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((dollars * 1.0e12).round() as u64)
}

/// Adds what OpenRouter says about `entry` to `info`. Missing or odd fields are left out.
pub(crate) fn add_openrouter_meta(info: &mut ModelInfo, entry: &Value) {
    let pricing = entry.get("pricing");
    let price = Pricing {
        input_micros_per_mtok: micros_per_mtok(pricing.and_then(|p| p.get("prompt"))),
        output_micros_per_mtok: micros_per_mtok(pricing.and_then(|p| p.get("completion"))),
        per_image_micros: None,
    };
    if price.input_micros_per_mtok.is_some() || price.output_micros_per_mtok.is_some() {
        info.pricing = Some(price);
    }
    info.released = entry
        .get("created")
        .and_then(Value::as_u64)
        .filter(|t| *t > 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_ai_core::ModelId;
    use serde_json::json;

    fn info() -> ModelInfo {
        ModelInfo::new(ModelId::new("openai/gpt-4").unwrap(), "gpt-4")
    }

    #[test]
    fn dollars_per_token_become_micros_per_million_tokens() {
        let mut m = info();
        add_openrouter_meta(
            &mut m,
            &json!({"created":1692901234,"pricing":{"prompt":"0.00003","completion":"0.00006","image":"0","request":"0"}}),
        );
        let p = m.pricing.unwrap();
        assert_eq!(p.input_micros_per_mtok, Some(30_000_000), "$30 per million");
        assert_eq!(p.output_micros_per_mtok, Some(60_000_000));
        assert_eq!(
            p.per_image_micros, None,
            "the image field is for image input"
        );
        assert_eq!(m.released, Some(1_692_901_234));
    }

    #[test]
    fn free_models_report_zero_not_nothing() {
        let mut m = info();
        add_openrouter_meta(&mut m, &json!({"pricing":{"prompt":"0","completion":"0"}}));
        assert_eq!(m.pricing.unwrap().input_micros_per_mtok, Some(0));
    }

    #[test]
    fn missing_odd_or_absurd_values_are_left_out() {
        for entry in [
            json!({}),
            json!({"pricing":null}),
            json!({"pricing":{"prompt":"free","completion":"-1"}}),
            json!({"pricing":{"prompt":"5","completion":"NaN"}}),
            json!({"pricing":{"prompt":true,"completion":[1]}}),
            json!({"created":"yesterday"}),
            json!({"created":0}),
        ] {
            let mut m = info();
            add_openrouter_meta(&mut m, &entry);
            assert_eq!((m.pricing.clone(), m.released), (None, None), "{entry}");
        }
    }

    #[test]
    fn one_good_price_is_kept_without_the_other() {
        let mut m = info();
        add_openrouter_meta(&mut m, &json!({"pricing":{"prompt":"0.000001"}}));
        let p = m.pricing.unwrap();
        assert_eq!(
            (p.input_micros_per_mtok, p.output_micros_per_mtok),
            (Some(1_000_000), None)
        );
    }
}
