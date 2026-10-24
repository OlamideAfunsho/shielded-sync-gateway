//! Deliberate corruption of compact blocks, for the step 4 demonstration.
//!
//! The pass-through proxy can be started with `--tamper-file`. Each line is
//! `<height> <kind>`; the file is re-read for every block request, so
//! emptying it "corrects the source" without a restart. Kinds:
//!
//! * `ciphertext` — flip a bit in the 52-byte compact ciphertext of every
//!   Sapling output and Orchard/Ironwood action in the block
//! * `epk`        — flip a bit in every ephemeral key
//! * `nullifier`  — flip a bit in every Sapling spend and action nullifier
//! * `omit-tx`    — drop every compact transaction from the block
//!
//! Commitments (cmu/cmx) are left alone in every case, so the note
//! commitment tree a wallet builds still matches the chain.

use std::path::Path;

use lwproto::rpc::CompactBlock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Ciphertext,
    Epk,
    Nullifier,
    OmitTx,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub height: u64,
    pub kind: Kind,
}

pub fn load(path: &Path) -> Vec<Rule> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    text.lines()
        .filter_map(|l| {
            let mut p = l.split_whitespace();
            let height = p.next()?.parse().ok()?;
            let kind = match p.next()? {
                "ciphertext" => Kind::Ciphertext,
                "epk" => Kind::Epk,
                "nullifier" => Kind::Nullifier,
                "omit-tx" => Kind::OmitTx,
                _ => return None,
            };
            Some(Rule { height, kind })
        })
        .collect()
}

/// Applies matching rules; returns how many fields were changed.
pub fn apply(rules: &[Rule], b: &mut CompactBlock) -> usize {
    let mut n = 0;
    for r in rules.iter().filter(|r| r.height == b.height) {
        match r.kind {
            Kind::OmitTx => {
                n += b.vtx.len();
                b.vtx.clear();
            }
            Kind::Ciphertext | Kind::Epk | Kind::Nullifier => {
                for tx in &mut b.vtx {
                    for o in &mut tx.outputs {
                        match r.kind {
                            Kind::Ciphertext => o.ciphertext[0] ^= 1,
                            Kind::Epk => o.ephemeral_key[0] ^= 1,
                            _ => continue,
                        }
                        n += 1;
                    }
                    if r.kind == Kind::Nullifier {
                        for s in &mut tx.spends {
                            s.nf[0] ^= 1;
                            n += 1;
                        }
                    }
                    for a in tx.actions.iter_mut().chain(tx.ironwood_actions.iter_mut()) {
                        match r.kind {
                            Kind::Ciphertext => a.ciphertext[0] ^= 1,
                            Kind::Epk => a.ephemeral_key[0] ^= 1,
                            Kind::Nullifier => a.nullifier[0] ^= 1,
                            Kind::OmitTx => {}
                        }
                        n += 1;
                    }
                }
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use lwproto::rpc::{CompactOrchardAction, CompactSaplingOutput, CompactTx};

    fn block() -> CompactBlock {
        CompactBlock {
            height: 7,
            vtx: vec![CompactTx {
                outputs: vec![CompactSaplingOutput {
                    cmu: vec![1; 32],
                    ephemeral_key: vec![2; 32],
                    ciphertext: vec![3; 52],
                }],
                ironwood_actions: vec![CompactOrchardAction {
                    nullifier: vec![4; 32],
                    cmx: vec![5; 32],
                    ephemeral_key: vec![6; 32],
                    ciphertext: vec![7; 52],
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn ciphertext_rule_keeps_commitments() {
        let mut b = block();
        let n = apply(
            &[Rule {
                height: 7,
                kind: Kind::Ciphertext,
            }],
            &mut b,
        );
        assert_eq!(n, 2);
        let tx = &b.vtx[0];
        assert_eq!(tx.outputs[0].cmu, vec![1; 32]);
        assert_eq!(tx.ironwood_actions[0].cmx, vec![5; 32]);
        assert_ne!(tx.outputs[0].ciphertext, vec![3; 52]);
        assert_ne!(tx.ironwood_actions[0].ciphertext, vec![7; 52]);
    }

    #[test]
    fn rules_for_other_heights_do_nothing() {
        let mut b = block();
        assert_eq!(
            apply(
                &[Rule {
                    height: 8,
                    kind: Kind::OmitTx
                }],
                &mut b
            ),
            0
        );
        assert_eq!(b.vtx.len(), 1);
    }
}
