use std::collections::VecDeque;
use std::ops::MulAssign;
use std::sync::Arc;
use std::sync::OnceLock;

use num_traits::One;

use super::b_field_element::BFieldElement;
use super::polynomial::Polynomial;
use super::traits::FiniteField;

#[derive(Debug, Clone)]
pub struct Leaf<FF: FiniteField + MulAssign<BFieldElement>> {
    pub points: Vec<FF>,
    zerofier: Polynomial<FF>,
}

#[derive(Debug, Clone)]
pub struct Branch<FF: FiniteField + MulAssign<BFieldElement>> {
    zerofier: Polynomial<FF>,
    left: ZerofierTree<FF>,
    right: ZerofierTree<FF>,
}

/// A zerofier tree is a balanced binary tree of vanishing polynomials.
/// Conceptually, every leaf corresponds to a single point, and the value of
/// that leaf is the monic linear polynomial that evaluates to zero there and
/// no-where else. Every non-leaf node is the product of its two children.
/// In practice, it makes sense to truncate the tree depth, in which case every
/// leaf contains a chunk of points whose size is upper-bounded and more or less
/// equal to some constant threshold.
#[derive(Debug, Clone)]
pub enum ZerofierTree<FF: FiniteField + MulAssign<BFieldElement>> {
    Leaf(Arc<OnceLock<Leaf<FF>>>),
    Branch(Arc<OnceLock<Branch<FF>>>),
    Padding,
}

impl<FF: FiniteField + MulAssign<BFieldElement>> ZerofierTree<FF> {
    /// Regulates the depth at which the tree is truncated. Phrased differently,
    /// regulates the number of points contained by each leaf.
    const ZEROFIER_TREE_RECURSION_CUTOFF_THRESHOLD: usize = 16;

    pub fn new_from_domain(domain: &[FF]) -> Self {
        let mut nodes = domain
            .chunks(Self::ZEROFIER_TREE_RECURSION_CUTOFF_THRESHOLD)
            .map(|chunk| {
                let zerofier = Polynomial::zerofier(chunk);
                let points = chunk.to_vec();
                let leaf = Leaf { zerofier, points };
                let leaf_once_lock = OnceLock::new();
                leaf_once_lock.get_or_init(|| leaf);
                ZerofierTree::Leaf(Arc::new(leaf_once_lock))
            })
            .collect::<VecDeque<_>>();
        nodes.resize(nodes.len().next_power_of_two(), ZerofierTree::Padding);
        while nodes.len() > 1 {
            let right = nodes.pop_back().unwrap();
            let left = nodes.pop_back().unwrap();
            if matches!(left, ZerofierTree::Padding) {
                nodes.push_front(ZerofierTree::Padding);
            } else {
                let zerofier = left.zerofier().multiply(&right.zerofier());
                let new_node = Branch {
                    zerofier,
                    left,
                    right,
                };
                let new_node_once_lock = OnceLock::new();
                new_node_once_lock.get_or_init(|| new_node);
                nodes.push_front(ZerofierTree::Branch(Arc::new(new_node_once_lock)));
            }
        }
        nodes.front().unwrap().clone()
    }

    pub fn zerofier(&self) -> Polynomial<FF> {
        match self {
            ZerofierTree::Leaf(leaf) => leaf.get().unwrap().zerofier.clone(),
            ZerofierTree::Branch(branch) => branch.get().unwrap().zerofier.clone(),
            ZerofierTree::Padding => Polynomial::<FF>::one(),
        }
    }

    pub fn left_branch(&self) -> Option<ZerofierTree<FF>> {
        match self {
            ZerofierTree::Branch(branch) => Some(branch.get().unwrap().left.clone()),
            _ => None,
        }
    }

    pub fn right_branch(&self) -> Option<ZerofierTree<FF>> {
        match self {
            ZerofierTree::Branch(branch) => Some(branch.get().unwrap().right.clone()),
            _ => None,
        }
    }

    pub fn leaf(&self) -> Option<Leaf<FF>> {
        match self {
            ZerofierTree::Leaf(leaf) => Some(leaf.get().unwrap().clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod test {
    use num_traits::Zero;
    use proptest::{collection::vec, prop_assert_eq};
    use proptest_arbitrary_interop::arb;
    use test_strategy::proptest;

    use crate::{
        math::zerofier_tree::ZerofierTree,
        prelude::{BFieldElement, Polynomial},
    };

    #[proptest]
    fn zerofier_tree_root_is_multiple_of_children(
        #[strategy(vec(arb(), 2usize*ZerofierTree::<BFieldElement>::ZEROFIER_TREE_RECURSION_CUTOFF_THRESHOLD))]
        points: Vec<BFieldElement>,
    ) {
        let zerofier_tree = ZerofierTree::new_from_domain(&points);
        prop_assert_eq!(
            Polynomial::zero(),
            zerofier_tree
                .zerofier()
                .reduce(&zerofier_tree.left_branch().unwrap().zerofier())
        );
        prop_assert_eq!(
            Polynomial::zero(),
            zerofier_tree
                .zerofier()
                .reduce(&zerofier_tree.right_branch().unwrap().zerofier())
        );
    }

    #[proptest]
    fn zerofier_tree_root_has_right_degree(
        #[strategy(vec(arb(), 1usize..(1<<10)))] points: Vec<BFieldElement>,
    ) {
        let zerofier_tree = ZerofierTree::new_from_domain(&points);
        prop_assert_eq!(points.len(), zerofier_tree.zerofier().degree() as usize);
    }

    #[proptest]
    fn zerofier_tree_root_zerofies(
        #[strategy(vec(arb(), 1usize..(1<<10)))] points: Vec<BFieldElement>,
        #[strategy(0usize..#points.len())] index: usize,
    ) {
        let zerofier_tree = ZerofierTree::new_from_domain(&points);
        prop_assert_eq!(
            BFieldElement::zero(),
            zerofier_tree.zerofier().evaluate(points[index])
        );
    }
}
