//! Client for the trusted validator (zebrad JSON-RPC).
//!
//! In the MVP trust model this node is the only source the gateway
//! believes: it supplies canonical headers, the chain tip and note-tree
//! state. It must be operated independently of the producer and indexer.

use anyhow::{anyhow, bail, Context};
use serde_json::{json, Value};
use ssg_verifier::TrustedHeader;

#[derive(Clone)]
pub struct Trusted {
    url: String,
    http: reqwest::Client,
    /// Response bytes received from the trusted node, for overhead numbers.
    pub bytes_in: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

/// Decodes a display-order (byte-reversed) hex hash into protocol order.
pub fn hash_from_display(s: &str) -> anyhow::Result<[u8; 32]> {
    let mut b: [u8; 32] = hex::decode(s)?
        .try_into()
        .map_err(|_| anyhow!("hash {s} is not 32 bytes"))?;
    b.reverse();
    Ok(b)
}

/// Per-pool tree state at one height, as the trusted node reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PoolTree {
    /// Legacy `CommitmentTree` encoding, hex, exactly as `z_gettreestate`
    /// returns it (and as lightwalletd/zaino pass it on). Empty if the pool
    /// is not active at this height.
    pub final_state: String,
    pub final_root: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TreeStates {
    pub height: u32,
    pub hash: String,
    pub time: u32,
    pub sapling: PoolTree,
    pub orchard: PoolTree,
    pub ironwood: PoolTree,
}

/// Number of leaves in a tree from its legacy `CommitmentTree` encoding:
/// `Option<left> Option<right> CompactSize(n) Option<parent>{n}` where an
/// option is a 0/1 byte followed by 32 bytes when present. Parent `i`
/// stands for 2^(i+1) leaves.
pub fn tree_size(hex_state: &str) -> anyhow::Result<u64> {
    if hex_state.is_empty() {
        return Ok(0);
    }
    let b = hex::decode(hex_state)?;
    let mut i = 0usize;
    let opt = |i: &mut usize| -> anyhow::Result<bool> {
        let flag = *b.get(*i).context("truncated tree state")?;
        *i += 1;
        match flag {
            0 => Ok(false),
            1 => {
                if *i + 32 > b.len() {
                    bail!("truncated tree node");
                }
                *i += 32;
                Ok(true)
            }
            f => bail!("bad option flag {f}"),
        }
    };
    let mut size = u64::from(opt(&mut i)?) + u64::from(opt(&mut i)?);
    let n = match *b.get(i).context("truncated tree state")? {
        v @ 0..=0xfc => {
            i += 1;
            u64::from(v)
        }
        0xfd => {
            let v = u16::from_le_bytes(b.get(i + 1..i + 3).context("truncated")?.try_into()?);
            i += 3;
            u64::from(v)
        }
        _ => bail!("implausible parent count"),
    };
    for level in 0..n {
        if opt(&mut i)? {
            size += 1u64 << (level + 1);
        }
    }
    if i != b.len() {
        bail!("{} trailing bytes in tree state", b.len() - i);
    }
    Ok(size)
}

impl Trusted {
    pub fn new(url: String) -> anyhow::Result<Self> {
        Ok(Trusted {
            url,
            http: reqwest::Client::builder().no_proxy().build()?,
            bytes_in: Default::default(),
        })
    }

    async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let bytes = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("trusted node {method}"))?
            .bytes()
            .await?;
        self.bytes_in
            .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
        let v: Value = serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "trusted node {method}: not JSON: {}",
                String::from_utf8_lossy(&bytes[..bytes.len().min(200)])
            )
        })?;
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            bail!("trusted node {method}: {e}");
        }
        Ok(v["result"].clone())
    }

    pub async fn chain_name(&self) -> anyhow::Result<String> {
        let v = self.call("getblockchaininfo", json!([])).await?;
        Ok(v["chain"].as_str().context("no chain name")?.to_string())
    }

    /// Best-chain tip (height, protocol-order hash).
    pub async fn tip(&self) -> anyhow::Result<(u32, [u8; 32])> {
        let v = self.call("getblockchaininfo", json!([])).await?;
        let height = v["blocks"].as_u64().context("no tip height")? as u32;
        let hash = hash_from_display(v["bestblockhash"].as_str().context("no tip hash")?)?;
        Ok((height, hash))
    }

    pub async fn header(&self, height: u32) -> anyhow::Result<TrustedHeader> {
        let v = self
            .call("getblockheader", json!([height.to_string(), true]))
            .await?;
        if v["height"].as_u64() != Some(u64::from(height)) {
            bail!(
                "trusted node returned header for height {} instead of {height}",
                v["height"]
            );
        }
        let prev_hash = match v["previousblockhash"].as_str() {
            Some(p) => hash_from_display(p)?,
            None => [0u8; 32],
        };
        Ok(TrustedHeader {
            height,
            hash: hash_from_display(v["hash"].as_str().context("no hash")?)?,
            prev_hash,
            merkle_root: hash_from_display(v["merkleroot"].as_str().context("no merkleroot")?)?,
            time: v["time"].as_u64().context("no time")? as u32,
        })
    }

    pub async fn tree_states(&self, height: u32) -> anyhow::Result<TreeStates> {
        let v = self
            .call("z_gettreestate", json!([height.to_string()]))
            .await?;
        let pool = |name: &str| -> anyhow::Result<PoolTree> {
            let c = &v[name]["commitments"];
            let final_state = c["finalState"].as_str().unwrap_or("").to_string();
            Ok(PoolTree {
                size: tree_size(&final_state)?,
                final_root: c["finalRoot"].as_str().unwrap_or("").to_string(),
                final_state,
            })
        };
        Ok(TreeStates {
            height: v["height"].as_u64().context("no height")? as u32,
            hash: v["hash"].as_str().context("no hash")?.to_string(),
            time: v["time"].as_u64().context("no time")? as u32,
            sapling: pool("sapling")?,
            orchard: pool("orchard")?,
            ironwood: pool("ironwood")?,
        })
    }

    /// `z_getsubtreesbyindex`: (root bytes, completing height) pairs.
    pub async fn subtrees(
        &self,
        pool: &str,
        start: u32,
        limit: Option<u32>,
    ) -> anyhow::Result<Vec<(Vec<u8>, u32)>> {
        let params = match limit {
            Some(l) => json!([pool, start, l]),
            None => json!([pool, start]),
        };
        let v = self.call("z_getsubtreesbyindex", params).await?;
        let mut out = vec![];
        for s in v["subtrees"].as_array().context("no subtrees")? {
            out.push((
                hex::decode(s["root"].as_str().context("no root")?)?,
                s["end_height"].as_u64().context("no end_height")? as u32,
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_sizes_from_real_zebrad_states() {
        // Regtest height 116 (results/step1): 2 Sapling leaves, empty
        // Orchard, 4 Ironwood leaves.
        let sapling = "01611b6f06c51755c617340038b5b4b8d0215d12f955a3d18004ee6742bdd44e6001a4a64a39bf191da3728421c859a94109a992d5db7f6fbe6d92990c11944990191f00000000000000000000000000000000000000000000000000000000000000";
        let ironwood = "01997ef93ae00d65522786c28a45677ca98a7d787d429d18029477c586f718140901977a9c8a664868912cd8684d62d3e2e99dcf35475260bd5b3dcdfa78bed9843a1f0128a80607200313796ef5062cdf0e4b7c47afc28e352959bcf2cc41acd639050c000000000000000000000000000000000000000000000000000000000000";
        assert_eq!(tree_size(sapling).unwrap(), 2);
        assert_eq!(tree_size("000000").unwrap(), 0);
        assert_eq!(tree_size(ironwood).unwrap(), 4);
        assert_eq!(tree_size("").unwrap(), 0);
        assert!(tree_size("02").is_err());
    }
}
