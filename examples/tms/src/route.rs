//! Route optimization for fleet dispatch.
//!
//! Two cooperating heuristics turn a list of stops into a visiting order:
//!
//! 1. [`nearest_neighbor_tour`] — a greedy `O(n²)` seed tour from the depot.
//! 2. [`two_opt_improve`] — local-search refinement. Each candidate edge swap is scored
//!    in **parallel** with [`rayon`], so a many-stop route improves quickly on multi-core
//!    hardware.
//!
//! [`tour_distance`] measures the full closed-loop length. The accompanying `ignored`
//! benchmark compares the optimized tour against a naive (insertion-order) tour and
//! asserts the optimized one is strictly shorter.

use rayon::prelude::*;

use crate::geo::{LatLng, haversine_km};

/// A stop to be visited. `id` is an opaque caller key; `pos` is its location.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stop {
    pub id: usize,
    pub pos: LatLng,
}

/// Euclidean (haversine) distance between two stops, in km.
pub fn dist(a: &Stop, b: &Stop) -> f64 {
    haversine_km(a.pos, b.pos)
}

/// Total length of a closed tour: depot → stops[0] → … → stops[n-1] → depot. `tour`
/// is a permutation of stop *indices* into `stops`.
pub fn tour_distance(stops: &[Stop], tour: &[usize]) -> f64 {
    if tour.is_empty() {
        return 0.0;
    }
    let depot = stops[0];
    let mut total = dist(&depot, &stops[tour[0]]);
    for w in tour.windows(2) {
        total += dist(&stops[w[0]], &stops[w[1]]);
    }
    total += dist(&stops[*tour.last().unwrap()], &depot);
    total
}

/// Greedy nearest-neighbor seed tour starting from the depot (`stops[0]`). Returns the
/// permutation of the remaining stop indices, optimized for insertion order. `O(n²)`.
pub fn nearest_neighbor_tour(stops: &[Stop]) -> Vec<usize> {
    if stops.len() <= 1 {
        return Vec::new();
    }
    let n = stops.len();
    let mut visited = vec![false; n];
    visited[0] = true;
    let mut tour = Vec::with_capacity(n - 1);
    let mut current = 0usize;
    for _ in 1..n {
        let mut best = None;
        for j in 1..n {
            if !visited[j] {
                let d = dist(&stops[current], &stops[j]);
                if best.map(|(_, bd)| d < bd).unwrap_or(true) {
                    best = Some((j, d));
                }
            }
        }
        if let Some((j, _)) = best {
            visited[j] = true;
            tour.push(j);
            current = j;
        }
    }
    tour
}

/// 2-opt local search over a tour, scoring each candidate swap in parallel with
/// [`rayon`]. Repeatedly applies the best improving swap until no improvement remains or
/// `max_passes` is reached. Returns the refined permutation.
pub fn two_opt_improve(stops: &[Stop], initial: Vec<usize>, max_passes: usize) -> Vec<usize> {
    let mut tour = initial;
    for _ in 0..max_passes {
        if tour.len() < 2 {
            break;
        }
        // Evaluate every (i,j) swap pair in parallel; keep the best that reduces length.
        let improvements: Vec<(usize, usize, f64)> = (0..tour.len())
            .into_par_iter()
            .flat_map(|i| (i + 1..tour.len()).map(move |j| (i, j)).collect::<Vec<_>>())
            .filter_map(|(i, j)| {
                let mut candidate = tour.clone();
                candidate[i..=j].reverse();
                let delta = tour_distance(stops, &candidate) - tour_distance(stops, &tour);
                if delta < -1e-9 {
                    Some((i, j, delta))
                } else {
                    None
                }
            })
            .collect();

        if improvements.is_empty() {
            break;
        }
        // Pick the single best improvement (parallel search returns many; the most
        // negative delta wins).
        let (bi, bj, _) = improvements
            .into_iter()
            .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap())
            .unwrap();
        tour[bi..=bj].reverse();
    }
    tour
}

/// Build a full optimized tour (seed + 2-opt) for `stops`.
pub fn optimize(stops: &[Stop], max_passes: usize) -> Vec<usize> {
    let seed = nearest_neighbor_tour(stops);
    two_opt_improve(stops, seed, max_passes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stops(n: usize) -> Vec<Stop> {
        (0..n)
            .map(|i| Stop {
                id: i,
                pos: LatLng::new(40.0 + (i as f64) * 0.01, -73.0 + (i as f64) * 0.01),
            })
            .collect()
    }

    #[test]
    fn tour_covers_every_stop_once() {
        let s = stops(8);
        let tour = optimize(&s, 10);
        let mut sorted = tour.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (1..8).collect::<Vec<_>>());
    }

    #[test]
    fn two_opt_never_increases_length() {
        let s = stops(12);
        let seed = nearest_neighbor_tour(&s);
        let seed_len = tour_distance(&s, &seed);
        let improved = two_opt_improve(&s, seed.clone(), 20);
        let imp_len = tour_distance(&s, &improved);
        assert!(imp_len <= seed_len + 1e-9, "{imp_len} > {seed_len}");
    }

    /// Benchmark vs. a naive insertion-order tour: the optimized tour must be shorter.
    /// Ignored in normal CI; run with `cargo test -p tms --release -- --ignored`.
    #[test]
    #[ignore]
    fn benchmark_vs_naive() {
        let s = stops(200);
        let naive: Vec<usize> = (1..s.len()).collect();
        let naive_len = tour_distance(&s, &naive);
        let optimized = optimize(&s, 50);
        let opt_len = tour_distance(&s, &optimized);
        println!("naive={naive_len:.3}km optimized={opt_len:.3}km");
        assert!(
            opt_len < naive_len,
            "optimized {opt_len} not shorter than naive {naive_len}"
        );
    }
}
