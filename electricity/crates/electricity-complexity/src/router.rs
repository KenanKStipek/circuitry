//! `core/router.py::route_model`: score to model, the rule about who
//! wins. Reuses `electricity-config`'s own `RoutingSettings::band_for`/
//! `band_named` (J1's port) rather than duplicating the band walk.

use electricity_config::{ComplexityBand, RoutingSettings};

/// A model the router chose, and the band row it read it off
/// (`core/router.py::RouteDecision`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouteDecision<'a> {
    pub model: &'a str,
    pub band: &'a ComplexityBand,
}

/// `route_model(score=..., settings=..., explicit=...)`
/// (`core/router.py:68-107`): the model *score* routes to, or `None`
/// when the router defers -- routing disabled, no bands, or an
/// explicit model with `respect_explicit` still in force.
pub fn route_model<'a>(
    score: f64,
    settings: &'a RoutingSettings,
    explicit: bool,
) -> Option<RouteDecision<'a>> {
    if !settings.enabled || settings.bands.is_empty() {
        return None;
    }
    if explicit && settings.respect_explicit {
        return None;
    }
    let band = settings.band_for(score)?;
    if band.model.is_empty() {
        return None;
    }
    Some(RouteDecision {
        model: &band.model,
        band,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(model: &str, max: Option<f64>, name: Option<&str>) -> ComplexityBand {
        ComplexityBand {
            model: model.to_string(),
            max,
            name: name.map(str::to_string),
        }
    }

    fn settings(
        enabled: bool,
        bands: Vec<ComplexityBand>,
        respect_explicit: bool,
    ) -> RoutingSettings {
        RoutingSettings {
            enabled,
            bands,
            respect_explicit,
        }
    }

    #[test]
    fn disabled_routing_defers() {
        let s = settings(false, vec![band("gpt-4", None, None)], true);
        assert_eq!(route_model(50.0, &s, false), None);
    }

    #[test]
    fn empty_bands_defers() {
        let s = settings(true, vec![], true);
        assert_eq!(route_model(50.0, &s, false), None);
    }

    #[test]
    fn explicit_with_respect_explicit_defers() {
        let s = settings(true, vec![band("gpt-4", None, None)], true);
        assert_eq!(route_model(50.0, &s, true), None);
    }

    #[test]
    fn explicit_without_respect_explicit_still_routes() {
        let s = settings(true, vec![band("gpt-4", None, None)], false);
        let decision = route_model(50.0, &s, true).unwrap();
        assert_eq!(decision.model, "gpt-4");
    }

    #[test]
    fn score_at_just_below_and_above_a_boundary() {
        let s = settings(
            true,
            vec![
                band("small", Some(50.0), Some("low")),
                band("big", None, Some("catch_all")),
            ],
            true,
        );
        assert_eq!(route_model(49.9, &s, false).unwrap().model, "small");
        assert_eq!(route_model(50.0, &s, false).unwrap().model, "small");
        assert_eq!(route_model(50.1, &s, false).unwrap().model, "big");
    }

    #[test]
    fn a_band_with_an_empty_model_defers() {
        let s = settings(true, vec![band("", None, None)], true);
        assert_eq!(route_model(10.0, &s, false), None);
    }
}
