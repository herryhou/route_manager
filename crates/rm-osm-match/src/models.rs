//! Emission / transition cost models (trait-isolated for tuning).

/// Point-to-candidate emission cost. Only relative costs matter.
pub trait EmissionModel: Sync + Send {
    fn emission_cost(&self, dist_m: f64) -> f64;
    /// Candidates beyond this radius are not considered.
    fn search_radius_m(&self) -> f64;
}

/// Newson–Krumm log-normal snap cost, constants dropped (monotone in d).
#[derive(Debug, Clone)]
pub struct NewsonKrummEmission {
    pub sigma_m: f64,
    pub radius_m: f64,
}

impl Default for NewsonKrummEmission {
    fn default() -> Self {
        Self {
            sigma_m: 10.0,
            radius_m: 200.0,
        }
    }
}

impl EmissionModel for NewsonKrummEmission {
    fn emission_cost(&self, dist_m: f64) -> f64 {
        0.5 * (dist_m / self.sigma_m).powi(2)
    }

    fn search_radius_m(&self) -> f64 {
        self.radius_m
    }
}

/// Inputs to a transition cost between two consecutive candidate states.
pub struct TransitionCtx {
    /// great-circle distance between the two path points
    pub gps_m: f64,
    /// network distance between the two projections
    pub net_m: f64,
    /// state2 travels the same segment opposite to state1
    pub uturn: bool,
    /// cosine of the turn angle at a shared node (1 = straight)
    pub turn_cos: f64,
    /// connection needed a multi-hop A* path (gap repair)
    pub multi_hop: bool,
}

pub trait TransitionModel: Sync + Send {
    fn transition_cost(&self, ctx: &TransitionCtx) -> f64;
}

/// |gps - net| / beta plus U-turn and turn-angle penalties.
#[derive(Debug, Clone)]
pub struct BasicTransition {
    pub beta_m: f64,
    pub uturn_penalty: f64,
    pub turn_penalty_scale: f64,
}

impl Default for BasicTransition {
    fn default() -> Self {
        Self {
            beta_m: 5.0,
            uturn_penalty: 10.0,
            turn_penalty_scale: 5.0,
        }
    }
}

impl TransitionModel for BasicTransition {
    fn transition_cost(&self, ctx: &TransitionCtx) -> f64 {
        let mut c = (ctx.gps_m - ctx.net_m).abs() / self.beta_m;
        if ctx.uturn {
            c += self.uturn_penalty;
        }
        c += (1.0 - ctx.turn_cos.clamp(-1.0, 1.0)) * 0.5 * self.turn_penalty_scale;
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emission_grows_quadratically_with_distance() {
        let e = NewsonKrummEmission::default();
        let c10 = e.emission_cost(10.0);
        let c20 = e.emission_cost(20.0);
        assert!(c10 > 0.0);
        assert!((c20 / c10 - 4.0).abs() < 1e-9, "quadratic in d");
        assert_eq!(e.search_radius_m(), 200.0);
    }

    #[test]
    fn transition_prefers_network_close_to_gps_and_penalises_uturn() {
        let t = BasicTransition::default();
        let straight = TransitionCtx {
            gps_m: 100.0,
            net_m: 100.0,
            uturn: false,
            turn_cos: 1.0,
            multi_hop: false,
        };
        let detour = TransitionCtx {
            gps_m: 100.0,
            net_m: 300.0,
            uturn: false,
            turn_cos: 1.0,
            multi_hop: false,
        };
        let uturn = TransitionCtx {
            gps_m: 100.0,
            net_m: 100.0,
            uturn: true,
            turn_cos: -1.0,
            multi_hop: false,
        };
        let sharp = TransitionCtx {
            gps_m: 100.0,
            net_m: 100.0,
            uturn: false,
            turn_cos: -1.0,
            multi_hop: false,
        };
        assert!(t.transition_cost(&straight) < t.transition_cost(&detour));
        assert!(t.transition_cost(&straight) < t.transition_cost(&uturn));
        assert!(t.transition_cost(&straight) < t.transition_cost(&sharp));
        assert!(t.transition_cost(&sharp) < t.transition_cost(&uturn));
        assert!(t.transition_cost(&straight) < 1e-9);
    }
}
