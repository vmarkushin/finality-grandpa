// Copyright 2018 Parity Technologies (UK) Ltd.
// This file is part of finality-afg.

// Polkadot is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Polkadot is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with finality-afg. If not, see <http://www.gnu.org/licenses/>.

//! Maintains the vote-graph of the blockchain.
//!
//! See docs on `VoteGraph` for more information.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use super::Chain;

#[derive(Debug)]
struct Entry<H, V> {
	number: usize,
	// ancestor hashes in reverse order, e.g. ancestors[0] is the parent
	// and the last entry is the hash of the parent vote-node.
	ancestors: Vec<H>,
	descendents: Vec<H>, // descendent vote-nodes
	cumulative_vote: V,
}

impl<H: Hash + PartialEq + Clone, V> Entry<H, V> {
	// whether the given hash, number pair is a direct ancestor of this node.
	// `None` signifies that the graph must be traversed further back.
	fn in_direct_ancestry(&self, hash: &H, number: usize) -> Option<bool> {
		if number >= self.number { return None }
		let offset = self.number - number - 1;

		self.ancestors.get(offset).map(|h| h == hash)
	}

	// get ancestor vote-node.
	fn ancestor_node(&self) -> Option<H> {
		self.ancestors.last().map(|x| x.clone())
	}
}

/// Maintains a DAG of blocks in the chain which have votes attached to them.
///
///
pub struct VoteGraph<H: Hash + Eq, V> {
	entries: HashMap<H, Entry<H, V>>,
	heads: HashSet<H>,
	base: H,
}

impl<H, V> VoteGraph<H, V> where
	H: Hash + Eq + Clone,
	V: ::std::ops::AddAssign + Default + Clone,
{
	fn new(base_hash: H, base_number: usize) -> Self {
		let mut entries = HashMap::new();
		entries.insert(base_hash.clone(), Entry {
			number: base_number,
			ancestors: Vec::new(),
			descendents: Vec::new(),
			cumulative_vote: V::default(),
		});

		let mut heads = HashSet::new();
		heads.insert(base_hash.clone());

		VoteGraph {
			entries,
			heads,
			base: base_hash,
		}
	}

	/// Insert a vote with given value into the graph at given hash and number.
	pub fn insert<C: Chain<H>>(&mut self, hash: H, number: usize, vote: V, chain: &C) {
		match self.find_containing_nodes(hash.clone(), number) {
			Some(containing) => if containing.is_empty() {
				self.append(hash.clone(), number, chain);
			} else {
				self.introduce_branch(containing, hash.clone(), number);
			},
			None => {}, // this entry already exists
		}

		// update cumulative vote data.
		// NOTE: below this point, there always exists a node with the given hash and number.
		let mut inspecting_hash = hash;
		loop {
			let active_entry = self.entries.get_mut(&inspecting_hash)
				.expect("vote-node and its ancestry always exist after initial phase; qed");

			active_entry.cumulative_vote += vote.clone();

			match active_entry.ancestor_node() {
				Some(parent) => { inspecting_hash = parent },
				None => break,
			}
		}
	}

	// attempts to find the containing node keys for the given hash and number.
	//
	// returns `None` if there is a node by that key already, and a vector
	// (potentially empty) of nodes with the given block in its ancestor-edge
	// otherwise.
	fn find_containing_nodes(&self, hash: H, number: usize) -> Option<Vec<H>> {
		if self.entries.contains_key(&hash) {
			return None
		}

		let mut containing_keys = Vec::new();
		let mut visited = HashSet::new();

		// iterate vote-heads and their ancestry backwards until we find the one with
		// this target hash in that chain.
		for mut head in self.heads.iter().cloned() {
			let mut active_entry;

			loop {
				active_entry = match self.entries.get(&head) {
					Some(e) => e,
					None => break,
				};

				// if node has been checked already, break
				if !visited.insert(head.clone()) { break }

				match active_entry.in_direct_ancestry(&hash, number) {
					Some(true) => {
						// set containing node and continue search.
						containing_keys.push(head.clone());
					}
					Some(false) => {}, // nothing in this branch. continue search.
					None => if let Some(prev) = active_entry.ancestor_node() {
						head = prev;
						continue // iterate backwards
					},
				}

				break
			}
		}

		Some(containing_keys)
	}

	// introduce a branch to given vote-nodes.
	//
	// `descendents` is a list of nodes with ancestor-edges containing the given ancestor.
	//
	// This function panics if any member of `descendents` is not a vote-node
	// or does not have ancestor with given hash and number OR if `ancestor_hash`
	// is already a known entry.
	fn introduce_branch(&mut self, descendents: Vec<H>, ancestor_hash: H, ancestor_number: usize) {
		let produced_entry = descendents.into_iter().fold(None, |mut maybe_entry, descendent| {
			let entry = self.entries.get_mut(&descendent)
				.expect("this function only invoked with keys of vote-nodes; qed");

			debug_assert!(entry.in_direct_ancestry(&ancestor_hash, ancestor_number).unwrap());

			// example: splitting number 10 at ancestor 4
			// before: [9 8 7 6 5 4 3 2 1]
			// after: [9 8 7 6 5 4], [3 2 1]
			// we ensure the `entry.ancestors` is drained regardless of whether
			// the `new_entry` has already been constructed.
			{
				let offset = entry.number.checked_sub(ancestor_number)
					.expect("this function only invoked with direct ancestors; qed");
				let new_ancestors = entry.ancestors.drain(offset..);

				let new_entry = maybe_entry.get_or_insert_with(move || Entry {
					number: ancestor_number,
					ancestors: new_ancestors.collect(),
					descendents: vec![],
					cumulative_vote: V::default(),
				});

				new_entry.descendents.push(descendent);
				new_entry.cumulative_vote += entry.cumulative_vote.clone();
			}

			maybe_entry
		});

		if let Some(new_entry) = produced_entry {
			assert!(
				self.entries.insert(ancestor_hash, new_entry).is_none(),
				"thus function is only invoked when there is no entry for the ancestor already; qed",
			)
		}
	}

	// append a vote-node onto the chain-tree. This should only be called if
	// no node in the tree keeps the target anyway.
	fn append<C: Chain<H>>(&mut self, hash: H, number: usize, chain: &C) {
		// TODO: "unknown block" error and propagate it.
		let mut ancestry = chain.ancestry(self.base.clone(), hash.clone()).unwrap();

		let mut ancestor_index = None;
		for (i, ancestor) in ancestry.iter().enumerate() {
			if let Some(entry) = self.entries.get_mut(ancestor) {
				entry.descendents.push(hash.clone());
				ancestor_index = Some(i);
				break;
			}
		}

		let ancestor_index = ancestor_index.expect("base is kept; \
			chain returns ancestry only if the block is a descendent of base; qed");

		let ancestor_hash = ancestry[ancestor_index].clone();
		ancestry.truncate(ancestor_index + 1);

		self.entries.insert(hash.clone(), Entry {
			number,
			ancestors: ancestry,
			descendents: Vec::new(),
			cumulative_vote: V::default(),
		});

		self.heads.remove(&ancestor_hash);
		self.heads.insert(hash);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const GENESIS_HASH: &str = "genesis";
	const NULL_HASH: &str = "NULL";

	struct BlockRecord {
		number: usize,
		parent: &'static str,
	}

	struct DummyChain {
		inner: HashMap<&'static str, BlockRecord>,
	}

	impl DummyChain {
		fn new() -> Self {
			let mut inner = HashMap::new();
			inner.insert(GENESIS_HASH, BlockRecord { number: 1, parent: NULL_HASH });

			DummyChain { inner }
		}

		fn push_blocks(&mut self, mut parent: &'static str, blocks: &[&'static str]) {
			let base_number = self.inner.get(parent).unwrap().number + 1;
			for (i, descendent) in blocks.iter().enumerate() {
				self.inner.insert(descendent, BlockRecord {
					number: base_number + i,
					parent,
				});

				parent = descendent;
			}
		}
	}

	impl Chain<&'static str> for DummyChain {
		fn ancestry(&self, base: &'static str, mut block: &'static str) -> Option<Vec<&'static str>> {
			let mut ancestry = Vec::new();

			loop {
				match self.inner.get(block) {
					None => return None,
					Some(record) => { block = record.parent; }
				}

				ancestry.push(block);

				if block == NULL_HASH { return None }
				if block == base { break }
			}

			Some(ancestry)
		}
	}

	#[test]
	fn graph_fork_not_at_node() {
		let mut chain = DummyChain::new();
		let mut tracker = VoteGraph::new(GENESIS_HASH, 1);

		chain.push_blocks(GENESIS_HASH, &["A", "B", "C"]);
		chain.push_blocks("C", &["D1", "E1", "F1"]);
		chain.push_blocks("C", &["D2", "E2", "F2"]);

		tracker.insert("A", 2, 100usize, &chain);
		tracker.insert("E1", 6, 100, &chain);
		tracker.insert("F2", 7, 100, &chain);

		assert!(tracker.heads.contains("E1"));
		assert!(tracker.heads.contains("F2"));
		assert!(!tracker.heads.contains("A"));

		let a_entry = tracker.entries.get("A").unwrap();
		assert_eq!(a_entry.descendents, vec!["E1", "F2"]);
		assert_eq!(a_entry.cumulative_vote, 300);


		let e_entry = tracker.entries.get("E1").unwrap();
		assert_eq!(e_entry.ancestor_node().unwrap(), "A");
		assert_eq!(e_entry.cumulative_vote, 100);

		let f_entry = tracker.entries.get("F2").unwrap();
		assert_eq!(f_entry.ancestor_node().unwrap(), "A");
		assert_eq!(f_entry.cumulative_vote, 100);
	}

	#[test]
	fn graph_fork_at_node() {
		let mut chain = DummyChain::new();
		let mut tracker1 = VoteGraph::new(GENESIS_HASH, 1);
		let mut tracker2 = VoteGraph::new(GENESIS_HASH, 1);

		chain.push_blocks(GENESIS_HASH, &["A", "B", "C"]);
		chain.push_blocks("C", &["D1", "E1", "F1"]);
		chain.push_blocks("C", &["D2", "E2", "F2"]);

		tracker1.insert("C", 4, 100usize, &chain);
		tracker1.insert("E1", 6, 100, &chain);
		tracker1.insert("F2", 7, 100, &chain);

		tracker2.insert("E1", 6, 100usize, &chain);
		tracker2.insert("F2", 7, 100, &chain);
		tracker2.insert("C", 4, 100, &chain);

		for tracker in &[&tracker2] {
			assert!(tracker.heads.contains("E1"));
			assert!(tracker.heads.contains("F2"));
			assert!(!tracker.heads.contains("C"));

			let c_entry = tracker.entries.get("C").unwrap();
			assert!(c_entry.descendents.contains(&"E1"));
			assert!(c_entry.descendents.contains(&"F2"));
			assert_eq!(c_entry.ancestor_node().unwrap(), GENESIS_HASH);
			assert_eq!(c_entry.cumulative_vote, 300);

			let e_entry = tracker.entries.get("E1").unwrap();
			assert_eq!(e_entry.ancestor_node().unwrap(), "C");
			assert_eq!(e_entry.cumulative_vote, 100);

			let f_entry = tracker.entries.get("F2").unwrap();
			assert_eq!(f_entry.ancestor_node().unwrap(), "C");
			assert_eq!(f_entry.cumulative_vote, 100);
		}
	}
}
