//! The pinned starting point for verification.
//!
//! Everything at or below the checkpoint is taken on trust; every block
//! above it is verified in order, starting from the checkpoint's hash and
//! tree sizes.

use std::path::Path;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::trusted::{PoolTree, Trusted};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Chain name as the trusted node reports it ("main", "test").
    pub network: String,
    pub height: u32,
    /// Display-order hex.
    pub hash: String,
    pub sapling: PoolTree,
    pub orchard: PoolTree,
    pub ironwood: PoolTree,
}

impl Checkpoint {
    pub async fn from_trusted(t: &Trusted, height: u32) -> anyhow::Result<Self> {
        let trees = t.tree_states(height).await?;
        let header = t.header(height).await?;
        let hash = ssg_verifier::verify::display(&header.hash);
        if hash != trees.hash {
            bail!("trusted node changed its block at {height} while building the checkpoint");
        }
        Ok(Checkpoint {
            network: t.chain_name().await?,
            height,
            hash,
            sapling: trees.sapling,
            orchard: trees.orchard,
            ironwood: trees.ironwood,
        })
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn hash_bytes(&self) -> anyhow::Result<[u8; 32]> {
        crate::trusted::hash_from_display(&self.hash)
    }

    pub fn sizes(&self) -> [u64; 3] {
        [self.sapling.size, self.orchard.size, self.ironwood.size]
    }

    /// Refuses to start against a trusted node that disagrees with the pin.
    pub async fn check_against(&self, t: &Trusted) -> anyhow::Result<()> {
        let network = t.chain_name().await?;
        if network != self.network {
            bail!(
                "checkpoint is for network {:?}, trusted node is on {network:?}",
                self.network
            );
        }
        let now = Checkpoint::from_trusted(t, self.height).await?;
        if now != *self {
            bail!(
                "trusted node disagrees with the checkpoint at height {} (hash {} vs pinned {})",
                self.height,
                now.hash,
                self.hash
            );
        }
        Ok(())
    }
}
