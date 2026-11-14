//! Forming matches from queue tickets (pure functions): the oldest ticket anchors a search, its rating window widens
//! with waiting time, teams shrink to the queue's `min_teams` after `shrink_after_s` per step, parties stay together,
//! and the most even split is chosen.

use crate::config::SearchWindow;
use crate::rating::{Rating, balance};

/// A server-evaluated entry condition of a queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requirement {
    /// Placement finished on a ladder (as many games as `rating.placement_games`).
    PlacementDone(String),
    /// At least `n` games on a ladder.
    MinGames(String, i64),
}

impl Requirement {
    pub fn parse(s: &str) -> Option<Requirement> {
        let parts: Vec<&str> = s.split(':').collect();
        match parts.as_slice() {
            ["placement_done", l] if crate::util::is_id(l) => Some(Requirement::PlacementDone(l.to_string())),
            ["min_games", l, n] if crate::util::is_id(l) => n
                .parse()
                .ok()
                .filter(|n| *n >= 0)
                .map(|n| Requirement::MinGames(l.to_string(), n)),
            _ => None,
        }
    }
}

/// A searching ticket as the matchmaker sees it.
#[derive(Debug, Clone)]
pub struct Cand {
    pub size: usize,
    pub rating: Rating,
    pub created_at: i64,
}

/// A formed match: the chosen candidates (indices), which go to team alpha, and the team sizes.
#[derive(Debug, Clone, PartialEq)]
pub struct Formed {
    pub picked: Vec<usize>,
    pub alpha: Vec<bool>,
    pub teams: [usize; 2],
}

fn window(s: &SearchWindow, waited: i64) -> f64 {
    (s.base + s.widen_per_s * waited.max(0) as f64).min(s.max)
}

pub struct Params<'a> {
    pub teams: [usize; 2],
    pub min_teams: [usize; 2],
    pub shrink_after_s: i64,
    pub search: &'a SearchWindow,
    pub center: f64,
}

/// Forms at most one match from `cands`.
pub fn form(cands: &[Cand], p: &Params<'_>, now: i64) -> Option<Formed> {
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by_key(|&i| (cands[i].created_at, i));
    for &anchor in &order {
        let a = &cands[anchor];
        let waited = now - a.created_at;
        let max_steps = p.teams[0] - p.min_teams[0];
        let steps = if p.shrink_after_s <= 0 {
            max_steps
        } else {
            ((waited / p.shrink_after_s).max(0) as usize).min(max_steps)
        };
        let mut pool: Vec<(f64, usize)> = order
            .iter()
            .copied()
            .filter(|&i| {
                let t = &cands[i];
                let gap = (t.rating.rating - a.rating.rating).abs();
                gap <= window(p.search, waited).min(window(p.search, now - t.created_at))
            })
            .map(|i| ((cands[i].rating.rating - a.rating.rating).abs(), i))
            .collect();
        pool.sort_by(|x, y| {
            x.0.total_cmp(&y.0)
                .then(cands[x.1].created_at.cmp(&cands[y.1].created_at))
        });
        pool.truncate(24);
        let pool: Vec<usize> = pool.into_iter().map(|(_, i)| i).collect();
        for step in 0..=steps {
            let ta = p.teams[0] - step;
            let tb = p.teams[1] - step;
            if let Some(f) = search(cands, &pool, anchor, ta, tb, p.center) {
                return Some(f);
            }
        }
    }
    None
}

/// Depth-first search over `pool` (closest first) for a set including `anchor` that fills `ta + tb` and splits.
fn search(cands: &[Cand], pool: &[usize], anchor: usize, ta: usize, tb: usize, center: f64) -> Option<Formed> {
    let need = ta + tb;
    let biggest = ta.max(tb);
    if cands[anchor].size > biggest {
        return None;
    }
    let others: Vec<usize> = pool
        .iter()
        .copied()
        .filter(|&i| i != anchor && cands[i].size <= biggest)
        .collect();
    let mut chosen = vec![anchor];
    let mut budget = 20_000usize;
    #[allow(clippy::too_many_arguments)]
    fn dfs(
        cands: &[Cand],
        others: &[usize],
        from: usize,
        chosen: &mut Vec<usize>,
        total: usize,
        need: usize,
        ta: usize,
        tb: usize,
        center: f64,
        budget: &mut usize,
    ) -> Option<Formed> {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        if total == need {
            let groups: Vec<(usize, Rating)> = chosen.iter().map(|&i| (cands[i].size, cands[i].rating)).collect();
            return balance(&groups, ta, tb, center).map(|alpha| Formed {
                picked: chosen.clone(),
                alpha,
                teams: [ta, tb],
            });
        }
        for k in from..others.len() {
            let i = others[k];
            if total + cands[i].size > need {
                continue;
            }
            chosen.push(i);
            if let Some(f) = dfs(
                cands,
                others,
                k + 1,
                chosen,
                total + cands[i].size,
                need,
                ta,
                tb,
                center,
                budget,
            ) {
                return Some(f);
            }
            chosen.pop();
        }
        None
    }
    dfs(
        cands,
        &others,
        0,
        &mut chosen,
        cands[anchor].size,
        need,
        ta,
        tb,
        center,
        &mut budget,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(size: usize, rating: f64, at: i64) -> Cand {
        Cand {
            size,
            rating: Rating {
                rating,
                deviation: 80.0,
                volatility: 0.06,
                games: 10,
            },
            created_at: at,
        }
    }

    fn params(teams: [usize; 2], min: [usize; 2], search: &SearchWindow) -> Params<'_> {
        Params {
            teams,
            min_teams: min,
            shrink_after_s: 60,
            search,
            center: 1500.0,
        }
    }

    #[test]
    fn duel_pairs_the_two_oldest_compatible() {
        let s = SearchWindow::default();
        let cands = vec![c(1, 1500.0, 0), c(1, 1510.0, 5), c(1, 3000.0, 1)];
        let f = form(&cands, &params([1, 1], [1, 1], &s), 10).unwrap();
        let mut picked = f.picked.clone();
        picked.sort();
        assert_eq!(picked, vec![0, 1]);
        assert_eq!(f.alpha.iter().filter(|a| **a).count(), 1);
        assert!(form(&cands[..1], &params([1, 1], [1, 1], &s), 10).is_none());
    }

    #[test]
    fn window_widens_with_waiting() {
        let s = SearchWindow {
            base: 100.0,
            widen_per_s: 5.0,
            max: 800.0,
        };
        let cands = vec![c(1, 1000.0, 0), c(1, 1400.0, 0)];
        assert!(
            form(&cands, &params([1, 1], [1, 1], &s), 0).is_none(),
            "gap 400 > window 100"
        );
        assert!(
            form(&cands, &params([1, 1], [1, 1], &s), 70).is_some(),
            "window 100 + 5 x 70 = 450"
        );
    }

    #[test]
    fn parties_stay_together_and_teams_shrink() {
        let s = SearchWindow::default();
        // A party of 2 and 6 solos fill 4 v 4.
        let mut cands = vec![c(2, 1500.0, 0)];
        cands.extend((0..6).map(|i| c(1, 1500.0 + i as f64, 1)));
        let f = form(&cands, &params([4, 4], [2, 2], &s), 2).unwrap();
        assert_eq!(f.teams, [4, 4]);
        assert_eq!(f.picked.iter().map(|&i| cands[i].size).sum::<usize>(), 8);
        // Only 4 players: nothing at first, 3 v 3 needs 6, 2 v 2 after two shrink steps (120 s).
        let few: Vec<Cand> = (0..4).map(|i| c(1, 1500.0, i)).collect();
        let p = params([4, 4], [2, 2], &s);
        assert!(form(&few, &p, 10).is_none());
        assert!(form(&few, &p, 70).is_none(), "one step: 3 v 3 needs six players");
        assert_eq!(form(&few, &p, 130).unwrap().teams, [2, 2]);
        // A party bigger than a team never fits.
        assert!(form(&[c(3, 1500.0, 0), c(1, 1500.0, 0)], &params([2, 2], [2, 2], &s), 0).is_none());
    }

    #[test]
    fn requirements_parse() {
        assert_eq!(
            Requirement::parse("placement_done:duel"),
            Some(Requirement::PlacementDone("duel".into()))
        );
        assert_eq!(
            Requirement::parse("min_games:duel:10"),
            Some(Requirement::MinGames("duel".into(), 10))
        );
        assert_eq!(Requirement::parse("min_games:duel:-1"), None);
        assert_eq!(Requirement::parse("rank:gold"), None);
    }
}
