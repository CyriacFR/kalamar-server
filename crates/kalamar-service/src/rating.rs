//! The reference server's rating system, one example of what the protocol allows (the protocol itself prescribes
//! no formula): Glicko-2 (Glickman, "Example of the Glicko-2 system", 2012) per ladder and season, team matches
//! rated against a composite opponent, placement matches, and tiers of the server's own design over a conservative
//! rating. The constants are configurable.

use std::f64::consts::PI;

use crate::config::RatingSection;

pub const SCALE: f64 = 173.7178;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rating {
    pub rating: f64,
    pub deviation: f64,
    pub volatility: f64,
    pub games: i64,
}

impl Rating {
    pub fn initial(p: &RatingSection) -> Rating {
        Rating {
            rating: p.initial_rating,
            deviation: p.initial_deviation,
            volatility: p.initial_volatility,
            games: 0,
        }
    }
    /// The value shown: rating minus `display_deviations` deviations.
    pub fn shown(&self, p: &RatingSection) -> f64 {
        self.rating - p.display_deviations * self.deviation
    }
}

fn g(phi: f64) -> f64 {
    1.0 / (1.0 + 3.0 * phi * phi / (PI * PI)).sqrt()
}

fn expected(mu: f64, mu_j: f64, phi_j: f64) -> f64 {
    1.0 / (1.0 + (-g(phi_j) * (mu - mu_j)).exp())
}

/// One Glicko-2 rating period for `player` against `(opponent, score)` results (score 1, 0.5 or 0).
pub fn update(player: Rating, results: &[(Rating, f64)], p: &RatingSection) -> Rating {
    let center = p.initial_rating;
    let mu = (player.rating - center) / SCALE;
    let phi = player.deviation / SCALE;
    let sigma = player.volatility;
    let tau = p.tau;
    if results.is_empty() {
        let phi_star = (phi * phi + sigma * sigma).sqrt();
        return Rating {
            deviation: (phi_star * SCALE).min(p.initial_deviation),
            ..player
        };
    }
    let mut v_inv = 0.0;
    let mut delta_sum = 0.0;
    for (opp, s) in results {
        let mu_j = (opp.rating - center) / SCALE;
        let phi_j = opp.deviation / SCALE;
        let e = expected(mu, mu_j, phi_j);
        let gj = g(phi_j);
        v_inv += gj * gj * e * (1.0 - e);
        delta_sum += gj * (s - e);
    }
    let v = 1.0 / v_inv;
    let delta = v * delta_sum;

    // Step 5: new volatility (Illinois algorithm).
    let a = (sigma * sigma).ln();
    let f = |x: f64| {
        let ex = x.exp();
        let d = phi * phi + v + ex;
        ex * (delta * delta - phi * phi - v - ex) / (2.0 * d * d) - (x - a) / (tau * tau)
    };
    let eps = 1e-6;
    let mut big_a = a;
    let mut big_b = if delta * delta > phi * phi + v {
        (delta * delta - phi * phi - v).ln()
    } else {
        let mut k = 1.0;
        while f(a - k * tau) < 0.0 && k < 1000.0 {
            k += 1.0;
        }
        a - k * tau
    };
    let mut fa = f(big_a);
    let mut fb = f(big_b);
    let mut iterations = 0;
    while (big_b - big_a).abs() > eps && iterations < 100 {
        let c = big_a + (big_a - big_b) * fa / (fb - fa);
        let fc = f(c);
        if fc * fb <= 0.0 {
            big_a = big_b;
            fa = fb;
        } else {
            fa /= 2.0;
        }
        big_b = c;
        fb = fc;
        iterations += 1;
    }
    let sigma_new = (big_a / 2.0).exp();
    let phi_star = (phi * phi + sigma_new * sigma_new).sqrt();
    let phi_new = 1.0 / (1.0 / (phi_star * phi_star) + 1.0 / v).sqrt();
    let mu_new = mu + phi_new * phi_new * delta_sum;
    let out = Rating {
        rating: mu_new * SCALE + center,
        deviation: (phi_new * SCALE).max(p.min_deviation),
        volatility: sigma_new,
        games: player.games + results.len() as i64,
    };
    if out.rating.is_finite() && out.deviation.is_finite() && out.volatility.is_finite() {
        out
    } else {
        player
    }
}

fn mean(xs: impl Iterator<Item = f64>) -> f64 {
    let (s, n) = xs.fold((0.0, 0usize), |(s, n), x| (s + x, n + 1));
    if n == 0 { 0.0 } else { s / n as f64 }
}

/// Probability that team `a` beats team `b` (team means, combined deviations).
pub fn win_probability(a: &[Rating], b: &[Rating], center: f64) -> f64 {
    let mu_a = (mean(a.iter().map(|r| r.rating)) - center) / SCALE;
    let mu_b = (mean(b.iter().map(|r| r.rating)) - center) / SCALE;
    let phi = (mean(a.iter().chain(b).map(|r| (r.deviation / SCALE).powi(2)))).sqrt();
    expected(mu_a, mu_b, phi)
}

/// Rates one team match: each player against a composite opponent whose gap to the player equals the gap between
/// the team means. `score_a` is 1.0 when team `a` won, 0.0 when it lost.
pub fn update_team_match(a: &[Rating], b: &[Rating], score_a: f64, p: &RatingSection) -> (Vec<Rating>, Vec<Rating>) {
    let side = |own: &[Rating], opp: &[Rating], score: f64| -> Vec<Rating> {
        let own_mean = mean(own.iter().map(|r| r.rating));
        let opp_mean = mean(opp.iter().map(|r| r.rating));
        let opp_dev = mean(opp.iter().map(|r| r.deviation * r.deviation)).sqrt();
        own.iter()
            .map(|pl| {
                let composite = Rating {
                    rating: opp_mean + (pl.rating - own_mean),
                    deviation: opp_dev,
                    volatility: p.initial_volatility,
                    games: 0,
                };
                update(*pl, &[(composite, score)], p)
            })
            .collect()
    };
    (side(a, b, score_a), side(b, a, 1.0 - score_a))
}

/// The tier index of a shown value (the last tier whose `min` it reaches; the first tier otherwise).
pub fn tier_index(p: &RatingSection, shown: f64) -> usize {
    p.tiers
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, t)| shown >= t.min)
        .map(|(i, _)| i)
        .next_back()
        .unwrap_or(0)
}

/// Splits `sizes` (groups that stay together, with their mean ratings) into teams of exactly `ta` and `tb` players
/// with the most even win probability. Exhaustive over assignments (at most 2^8 for 8 groups). `None` when no
/// assignment fits.
pub fn balance(groups: &[(usize, Rating)], ta: usize, tb: usize, center: f64) -> Option<Vec<bool>> {
    let n = groups.len();
    if n == 0 || n > 16 {
        return None;
    }
    let mut best: Option<(f64, u32)> = None;
    for mask in 0u32..(1u32 << n) {
        let size_a: usize = (0..n).filter(|i| mask >> i & 1 == 1).map(|i| groups[i].0).sum();
        let size_b: usize = (0..n).filter(|i| mask >> i & 1 == 0).map(|i| groups[i].0).sum();
        if size_a != ta || size_b != tb {
            continue;
        }
        let expand = |want: u32| -> Vec<Rating> {
            (0..n)
                .filter(|i| mask >> i & 1 == want)
                .flat_map(|i| std::iter::repeat_n(groups[i].1, groups[i].0))
                .collect()
        };
        let gap = (win_probability(&expand(1), &expand(0), center) - 0.5).abs();
        if best.is_none_or(|(g, _)| gap < g - 1e-12) {
            best = Some((gap, mask));
        }
    }
    let (_, mask) = best?;
    Some((0..n).map(|i| mask >> i & 1 == 1).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RatingSection {
        RatingSection::default()
    }

    #[test]
    fn glickman_paper_example() {
        let p = Rating {
            rating: 1500.0,
            deviation: 200.0,
            volatility: 0.06,
            games: 0,
        };
        let opp = |r, d| Rating {
            rating: r,
            deviation: d,
            volatility: 0.06,
            games: 0,
        };
        let out = update(
            p,
            &[
                (opp(1400.0, 30.0), 1.0),
                (opp(1550.0, 100.0), 0.0),
                (opp(1700.0, 300.0), 0.0),
            ],
            &cfg(),
        );
        assert!((out.rating - 1464.06).abs() < 0.01, "{}", out.rating);
        assert!((out.deviation - 151.52).abs() < 0.01, "{}", out.deviation);
        assert!((out.volatility - 0.05999).abs() < 0.00001, "{}", out.volatility);
        assert_eq!(out.games, 3);
    }

    #[test]
    fn inactivity_grows_deviation_but_caps() {
        let p = Rating {
            deviation: 100.0,
            ..Rating::initial(&cfg())
        };
        let q = update(p, &[], &cfg());
        assert!(q.deviation > 100.0 && q.rating == p.rating);
    }

    #[test]
    fn team_match_moves_in_the_right_direction() {
        let c = cfg();
        let team = |r: f64| {
            vec![
                Rating {
                    rating: r,
                    ..Rating::initial(&c)
                };
                4
            ]
        };
        let (a, b) = update_team_match(&team(1500.0), &team(1500.0), 1.0, &c);
        assert!(a.iter().all(|r| r.rating > 1500.0));
        assert!(b.iter().all(|r| r.rating < 1500.0));
        let (upset, _) = update_team_match(&team(1300.0), &team(1700.0), 1.0, &c);
        let (expected_win, _) = update_team_match(&team(1700.0), &team(1300.0), 1.0, &c);
        assert!(upset[0].rating - 1300.0 > expected_win[0].rating - 1700.0);
        assert!((win_probability(&team(1500.0), &team(1500.0), 1500.0) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn tiers_and_balance() {
        let c = cfg();
        assert_eq!(c.tiers[tier_index(&c, 0.0)].id, "rookie");
        assert_eq!(c.tiers[tier_index(&c, 1400.0)].id, "gold");
        assert_eq!(c.tiers[tier_index(&c, 5000.0)].id, "master");
        let r = |x: f64| Rating {
            rating: x,
            deviation: 80.0,
            volatility: 0.06,
            games: 10,
        };
        // Eight solo players: perfect split exists (2000+1000+1500+1400 = 5900 per side).
        let groups: Vec<(usize, Rating)> = [2000.0, 1900.0, 1500.0, 1500.0, 1400.0, 1400.0, 1100.0, 1000.0]
            .map(|x| (1, r(x)))
            .to_vec();
        let split = balance(&groups, 4, 4, 1500.0).unwrap();
        let sum: f64 = groups
            .iter()
            .zip(&split)
            .filter(|(_, a)| **a)
            .map(|(g, _)| g.1.rating)
            .sum();
        assert_eq!(sum, 5900.0);
        // A party of 2 stays on one side; a party of 5 cannot fit 4 v 4.
        let with_party = vec![
            (2, r(1500.0)),
            (1, r(1500.0)),
            (1, r(1500.0)),
            (1, r(1500.0)),
            (1, r(1500.0)),
            (1, r(1500.0)),
        ];
        assert!(balance(&with_party, 4, 3, 1500.0).is_some());
        assert!(balance(&[(5, r(1500.0)), (3, r(1500.0))], 4, 4, 1500.0).is_none());
        assert!(balance(&[], 1, 1, 1500.0).is_none());
    }
}
