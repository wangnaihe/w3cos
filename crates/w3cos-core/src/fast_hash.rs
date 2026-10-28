//! Fast, non-DoS-resistant hash tables for the engine's own key tables.
//!
//! Property names, module specifiers and binding names are hashed on every
//! lookup. SipHash (`std`'s default) costs far more than the lookup itself on
//! short strings; Fx is several times cheaper. These tables are internal and
//! never keyed by attacker-controlled hasher state, so a collision only costs
//! lookup time.

use std::collections::{HashMap, HashSet};

pub type FastMap<K, V> = HashMap<K, V, rustc_hash::FxBuildHasher>;
pub type FastSet<K> = HashSet<K, rustc_hash::FxBuildHasher>;
