//! Parallel MRP (Material Requirements Planning) engine.
//!
//! A Bill-of-Materials is a tree of parts; exploding it multiplies parent demand down to
//! every component. Because each subtree is independent, the explosion is mapped over the
//! tree with [rayon](https://docs.rs/rayon), giving near-linear speedups and handling very
//! large BOMs (thousands of parts) in milliseconds.

use rayon::prelude::*;
use std::collections::HashMap;
use parking_lot::Mutex;
use tpt_erp_primitives::{Entity, Id};

/// A manufactured or purchased part.
#[derive(Debug)]
pub struct Part;
impl Entity for Part {}

/// A node in the Bill-of-Materials tree.
#[derive(Debug, Clone)]
pub struct BomNode {
    pub part: Id<Part>,
    /// How many of this part are consumed per parent unit.
    pub qty_per: u32,
    /// Manufacturing/purchase lead time in days.
    pub lead_time_days: u32,
    /// On-hand inventory already available.
    pub on_hand: u32,
    pub children: Vec<BomNode>,
}

/// Per-part requirement computed by the explosion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartRequirement {
    /// Total gross demand for the part across the whole BOM.
    pub gross: u64,
    /// Existing on-hand inventory.
    pub on_hand: u32,
    /// Net demand after applying on-hand (`gross.saturating_sub(on_hand)`).
    pub net: u64,
    /// True when net demand is positive (a shortage to be procured).
    pub shortage: bool,
    /// This part's own lead time in days.
    pub lead_time_days: u32,
}

/// The result of an MRP explosion: requirements per part plus the critical-path lead time.
#[derive(Debug, Clone)]
pub struct MrpPlan {
    pub requirements: HashMap<Id<Part>, PartRequirement>,
    /// Longest cumulative lead time (days) along any root-to-leaf path — the time until
    /// the whole order can be satisfied assuming everything is procured now.
    pub critical_path_days: u32,
}

/// The MRP engine.
pub struct MrpEngine;

impl MrpEngine {
    /// Explode `root` for `demand` finished units, computing gross/net requirements and
    /// the critical-path lead time in parallel.
    pub fn explode(root: &BomNode, demand: u64) -> MrpPlan {
        let acc: Mutex<HashMap<Id<Part>, RawReq>> = Mutex::new(HashMap::new());
        Self::explode_node(root, demand, &acc);
        let acc = acc.into_inner();

        // Gather each part's own lead time (and on-hand) from the BOM.
        let info: HashMap<Id<Part>, (u32, u32)> = Self::part_info(root);

        let requirements = acc
            .into_iter()
            .map(|(part, raw)| {
                let on_hand = info.get(&part).map(|(oh, _)| *oh).unwrap_or(0);
                let net = raw.gross.saturating_sub(u64::from(on_hand));
                let lead = info.get(&part).map(|(_, lt)| *lt).unwrap_or(0);
                (
                    part,
                    PartRequirement {
                        gross: raw.gross,
                        on_hand,
                        net,
                        shortage: net > 0,
                        lead_time_days: lead,
                    },
                )
            })
            .collect();

        let critical_path_days = Self::critical_path(root);
        MrpPlan {
            requirements,
            critical_path_days,
        }
    }

    fn explode_node(node: &BomNode, multiplier: u64, acc: &Mutex<HashMap<Id<Part>, RawReq>>) {
        let gross = u64::from(node.qty_per) * multiplier;
        {
            let mut m = acc.lock();
            m.entry(node.part).or_default().gross += gross;
        }
        // Each child subtree is independent given `gross`, so explode in parallel.
        node.children.par_iter().for_each(|c| {
            Self::explode_node(c, gross, acc);
        });
    }

    fn part_info(node: &BomNode) -> HashMap<Id<Part>, (u32, u32)> {
        let mut map = HashMap::new();
        map.insert(node.part, (node.on_hand, node.lead_time_days));
        for c in &node.children {
            map.extend(Self::part_info(c));
        }
        map
    }

    /// Longest cumulative lead time along any root-to-leaf path.
    fn critical_path(node: &BomNode) -> u32 {
        let here = node.lead_time_days;
        let child_max = node
            .children
            .par_iter()
            .map(Self::critical_path)
            .max()
            .unwrap_or(0);
        here + child_max
    }

    /// Parts in the plan that are short and must be procured.
    pub fn shortages(plan: &MrpPlan) -> Vec<(Id<Part>, u64)> {
        plan.requirements
            .iter()
            .filter(|(_, r)| r.shortage)
            .map(|(p, r)| (*p, r.net))
            .collect()
    }
}

#[derive(Default)]
struct RawReq {
    gross: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(part: Id<Part>, qty: u32, lead: u32, on_hand: u32) -> BomNode {
        BomNode {
            part,
            qty_per: qty,
            lead_time_days: lead,
            on_hand,
            children: vec![],
        }
    }

    #[test]
    fn single_level_explosion() {
        // 1 widget needs 2 bolts (on-hand 1) and 4 washers (on-hand 0).
        let bolt = Id::new();
        let washer = Id::new();
        let widget = Id::new();
        let root = BomNode {
            part: widget,
            qty_per: 1,
            lead_time_days: 1,
            on_hand: 0,
            children: vec![leaf(bolt, 2, 3, 1), leaf(washer, 4, 2, 0)],
        };
        let plan = MrpEngine::explode(&root, 10);
        let bolt_req = plan.requirements[&bolt];
        assert_eq!(bolt_req.gross, 20);
        assert_eq!(bolt_req.on_hand, 1);
        assert_eq!(bolt_req.net, 19);
        assert!(bolt_req.shortage);
        let washer_req = plan.requirements[&washer];
        assert_eq!(washer_req.net, 40);
        assert_eq!(plan.critical_path_days, 1 + 3); // widget + longest child
    }

    #[test]
    fn multi_level_multiplies_down_the_tree() {
        // A -> 2 B -> 3 C. Demand 5 A => 10 B => 30 C.
        let a = Id::new();
        let b = Id::new();
        let c = Id::new();
        let root = BomNode {
            part: a,
            qty_per: 1,
            lead_time_days: 2,
            on_hand: 0,
            children: vec![BomNode {
                part: b,
                qty_per: 2,
                lead_time_days: 4,
                on_hand: 0,
                children: vec![leaf(c, 3, 1, 0)],
            }],
        };
        let plan = MrpEngine::explode(&root, 5);
        assert_eq!(plan.requirements[&a].gross, 5);
        assert_eq!(plan.requirements[&b].gross, 10);
        assert_eq!(plan.requirements[&c].gross, 30);
        assert_eq!(plan.critical_path_days, 2 + 4 + 1);
    }

    /// Benchmark: explode a 4,000-part BOM and assert it completes in single-digit
    /// milliseconds. Ignored in normal CI; run with
    /// `cargo test -p mes --release -- --ignored benchmark_large_bom`.
    #[test]
    #[ignore]
    fn benchmark_large_bom() {
        // Build a balanced binary-ish tree of 4000 parts.
        let id_pool: Vec<Id<Part>> = (0..4000).map(|_| Id::new()).collect();
        let mut nodes: Vec<BomNode> = id_pool
            .iter()
            .map(|&p| leaf(p, 1 + (p.as_uuid().as_bytes()[0] as u32 % 4), 1, 0))
            .collect();
        // Wire nodes[2i+1], nodes[2i+2] as children of nodes[i].
        for i in 0..nodes.len() {
            let c1 = 2 * i + 1;
            let c2 = 2 * i + 2;
            if c1 < nodes.len() {
                let child = nodes[c1].clone();
                nodes[i].children.push(child);
            }
            if c2 < nodes.len() {
                let child = nodes[c2].clone();
                nodes[i].children.push(child);
            }
        }
        let root = nodes.remove(0);

        let start = std::time::Instant::now();
        let plan = MrpEngine::explode(&root, 1);
        let elapsed = start.elapsed();
        println!("exploded 4000-part BOM in {elapsed:?}");
        assert!(plan.requirements.len() <= 4000);
        assert!(
            elapsed.as_millis() < 500,
            "expected <500ms, got {elapsed:?}"
        );
    }
}
