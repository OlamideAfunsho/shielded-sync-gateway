//! The block header's transaction Merkle root (`hashMerkleRoot`).
//!
//! Zcash keeps Bitcoin's tree: leaves are txids in block order (protocol
//! byte order), each internal node is SHA-256d of the two children, and a
//! level with an odd count duplicates its last node. Since NU5 the leaves
//! for v5+ transactions are ZIP 244 txids.
//!
//! The duplication rule makes the tree ambiguous: a list ending in
//! `[.., a, b, c]` and one ending in `[.., a, b, c, c]` have the same root
//! (CVE-2012-2459). We reject any list where a level contains two equal
//! adjacent nodes in a pair, which is the same test Bitcoin and zcashd use
//! to flag a "mutated" block, and we reject duplicate txids outright.

use sha2::{Digest, Sha256};

fn sha256d(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let first = Sha256::new().chain_update(a).chain_update(b).finalize();
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

#[derive(Debug, PartialEq, Eq)]
pub enum MerkleError {
    Empty,
    /// Two equal nodes formed a pair at this level (0 = leaves).
    Mutated {
        level: usize,
        index: usize,
    },
}

/// Computes the root, refusing ambiguous (mutated) trees.
pub fn root(txids: &[[u8; 32]]) -> Result<[u8; 32], MerkleError> {
    if txids.is_empty() {
        return Err(MerkleError::Empty);
    }
    let mut level: Vec<[u8; 32]> = txids.to_vec();
    let mut depth = 0;
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0;
        while i < level.len() {
            let left = &level[i];
            let right = if i + 1 < level.len() {
                if level[i + 1] == *left {
                    return Err(MerkleError::Mutated {
                        level: depth,
                        index: i,
                    });
                }
                &level[i + 1]
            } else {
                left
            };
            next.push(sha256d(left, right));
            i += 2;
        }
        level = next;
        depth += 1;
    }
    Ok(level[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(n: u8) -> [u8; 32] {
        [n; 32]
    }

    #[test]
    fn single_leaf_is_its_own_root() {
        assert_eq!(root(&[leaf(7)]), Ok(leaf(7)));
    }

    #[test]
    fn odd_count_duplicates_last() {
        let r3 = root(&[leaf(1), leaf(2), leaf(3)]).unwrap();
        let manual = sha256d(&sha256d(&leaf(1), &leaf(2)), &sha256d(&leaf(3), &leaf(3)));
        assert_eq!(r3, manual);
    }

    #[test]
    fn duplicated_tail_is_rejected_not_accepted_as_same_root() {
        // [1,2,3] and [1,2,3,3] hash to the same root; only the first is a
        // valid transaction list.
        assert!(root(&[leaf(1), leaf(2), leaf(3)]).is_ok());
        assert_eq!(
            root(&[leaf(1), leaf(2), leaf(3), leaf(3)]),
            Err(MerkleError::Mutated { level: 0, index: 2 })
        );
    }

    #[test]
    fn mutation_detected_at_inner_level() {
        // Six leaves: [1..4,5,6] vs [1..4,5,6,5,6] collide at level 1.
        let base = [leaf(1), leaf(2), leaf(3), leaf(4), leaf(5), leaf(6)];
        let mut longer = base.to_vec();
        longer.extend_from_slice(&[leaf(5), leaf(6)]);
        assert!(root(&base).is_ok());
        assert!(matches!(
            root(&longer),
            Err(MerkleError::Mutated { level: 1, .. })
        ));
    }
}
