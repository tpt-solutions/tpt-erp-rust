//! Wave & route optimization for pickers.
//!
//! Given a set of bin locations to visit from a depot, the engine compares several
//! picker-path strategies and reports the total travel distance. The classic
//! S-shaped and nearest-neighbor heuristics consistently beat the naive "visit in
//! arrival order" path, which is what makes wave picking faster than ad-hoc picking.

use tpt_erp_primitives::{Entity, Id};

/// A physical pick location in the warehouse grid. `x` is the aisle, `y` the position
/// along the aisle.
#[derive(Debug, Clone)]
pub struct BinLocation {
    pub id: Id<Bin>,
    pub x: i32,
    pub y: i32,
}

/// A marker for warehouse bins (shared with the inventory engine's [`crate::inventory`]
/// module's `Bin` concept; re-declared here to keep `picking` dependency-free of it).
#[derive(Debug)]
pub struct Bin;
impl Entity for Bin {}

fn dist(ax: i32, ay: i32, bx: i32, by: i32) -> f64 {
    let dx = (ax - bx) as f64;
    let dy = (ay - by) as f64;
    (dx * dx + dy * dy).sqrt()
}

/// Total travel distance of an ordered route that starts and ends at the depot `(0,0)`.
pub fn route_distance(locs: &[BinLocation], order: &[usize]) -> f64 {
    let mut total = 0.0;
    let mut cx = 0;
    let mut cy = 0;
    for &i in order {
        let l = &locs[i];
        total += dist(cx, cy, l.x, l.y);
        cx = l.x;
        cy = l.y;
    }
    total += dist(cx, cy, 0, 0);
    total
}

/// Naive route: visit locations in the order they were handed over. This is the
/// baseline the smarter heuristics should beat.
pub fn naive_route(locs: &[BinLocation]) -> Vec<usize> {
    (0..locs.len()).collect()
}

/// Nearest-neighbor route: from the depot, repeatedly jump to the closest unvisited
/// location. Greedy, fast, and usually far shorter than naive.
pub fn nearest_neighbor_route(locs: &[BinLocation]) -> Vec<usize> {
    let n = locs.len();
    let mut visited = vec![false; n];
    let mut order = Vec::with_capacity(n);
    let (mut cx, mut cy) = (0, 0);
    for _ in 0..n {
        let mut best = None;
        let mut best_d = f64::MAX;
        for (i, _) in locs.iter().enumerate() {
            if visited[i] {
                continue;
            }
            let d = dist(cx, cy, locs[i].x, locs[i].y);
            if d < best_d {
                best_d = d;
                best = Some(i);
            }
        }
        let i = best.unwrap();
        visited[i] = true;
        order.push(i);
        cx = locs[i].x;
        cy = locs[i].y;
    }
    order
}

/// Batch/zone route: sort all locations by aisle then position. Mimics a picker walking
/// each aisle in order — a decent heuristic that ignores the return-to-depot jump
/// between aisles but is cheap to compute.
pub fn batch_route(locs: &[BinLocation]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..locs.len()).collect();
    order.sort_by(|&a, &b| (locs[a].x, locs[a].y).cmp(&(locs[b].x, locs[b].y)));
    order
}

/// S-shaped route: visit aisles in order, snaking up one aisle and down the next. This
/// is the textbook warehouse picker path and typically the shortest for dense aisles.
pub fn s_shape_route(locs: &[BinLocation]) -> Vec<usize> {
    let n = locs.len();
    // Group indices by aisle.
    let mut by_aisle: std::collections::BTreeMap<i32, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, l) in locs.iter().enumerate() {
        by_aisle.entry(l.x).or_default().push(i);
    }
    let mut order = Vec::with_capacity(n);
    for (aisle_idx, (_aisle, mut members)) in by_aisle.into_iter().enumerate() {
        members.sort_by_key(|&i| locs[i].y);
        // Snake: even aisles ascend, odd aisles descend.
        if aisle_idx % 2 == 1 {
            members.reverse();
        }
        order.extend(members);
    }
    order
}

/// Summary of a route-planning comparison across all strategies.
#[derive(Debug, Clone)]
pub struct RouteComparison {
    pub naive: f64,
    pub nearest_neighbor: f64,
    pub batch: f64,
    pub s_shape: f64,
    /// The name of the strategy that produced the shortest path.
    pub best: &'static str,
}

/// Plan and compare all picker-path strategies for one pick list.
pub fn compare_routes(locs: &[BinLocation]) -> RouteComparison {
    let naive = route_distance(locs, &naive_route(locs));
    let nn = route_distance(locs, &nearest_neighbor_route(locs));
    let batch = route_distance(locs, &batch_route(locs));
    let s = route_distance(locs, &s_shape_route(locs));
    let best = [
        ("naive", naive),
        ("nearest_neighbor", nn),
        ("batch", batch),
        ("s_shape", s),
    ]
    .into_iter()
    .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
    .map(|(name, _)| name)
    .unwrap();
    RouteComparison {
        naive,
        nearest_neighbor: nn,
        batch,
        s_shape: s,
        best,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_locs(n: usize, seed: u64) -> Vec<BinLocation> {
        // Deterministic LCG so benchmarks/tests are reproducible.
        let mut state = seed.wrapping_add(0x9E37_79B9);
        let mut rng = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };
        (0..n)
            .map(|_| BinLocation {
                id: Id::new(),
                x: (rng() % 20) as i32,
                y: (rng() % 40) as i32,
            })
            .collect()
    }

    #[test]
    fn heuristics_are_no_worse_than_naive() {
        let locs = random_locs(200, 42);
        let c = compare_routes(&locs);
        assert!(c.nearest_neighbor <= c.naive + 1e-9);
        assert!(c.s_shape <= c.naive + 1e-9);
        assert!(c.batch <= c.naive + 1e-9);
    }

    #[test]
    fn s_shape_wins_on_dense_single_aisle() {
        // All in one aisle, but the pick list arrives in a zig-zag order so the naive
        // route bounces up and down. S-shape sorts monotonically and walks straight.
        let ys = [0, 45, 5, 40, 10, 35, 15, 30, 20, 25];
        let locs: Vec<BinLocation> = ys
            .iter()
            .map(|&y| BinLocation {
                id: Id::new(),
                x: 3,
                y,
            })
            .collect();
        let c = compare_routes(&locs);
        assert!(c.s_shape < c.naive);
    }

    /// Benchmark: plan routes for a large pick list and compare wall-clock cost and
    /// quality of each strategy. Runs as an ignored test so it does not slow CI; run with
    /// `cargo test -p wms --release -- --ignored benchmark_large_pick_list`.
    #[test]
    #[ignore]
    fn benchmark_large_pick_list() {
        let locs = random_locs(5_000, 7);
        let start = std::time::Instant::now();
        let naive = route_distance(&locs, &naive_route(&locs));
        let t_naive = start.elapsed();

        let start = std::time::Instant::now();
        let nn = route_distance(&locs, &nearest_neighbor_route(&locs));
        let t_nn = start.elapsed();

        let start = std::time::Instant::now();
        let s = route_distance(&locs, &s_shape_route(&locs));
        let t_s = start.elapsed();

        println!("naive : dist={naive:.1}  time={t_naive:?}");
        println!("nn    : dist={nn:.1}  time={t_nn:?}");
        println!("s-shape: dist={s:.1}  time={t_s:?}");
        assert!(nn <= naive);
        assert!(s <= naive);
    }
}
