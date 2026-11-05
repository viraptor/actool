//! BOM (Bill of Materials) file writer.
//!
//! BOM is Apple's container format used for `.car` files and `.pkg`
//! installers. All structures are big-endian.

use byteorder::{BigEndian, WriteBytesExt};
use indexmap::IndexMap;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

pub struct BomWriter {
    blocks: Vec<Vec<u8>>,
    named_blocks: IndexMap<String, usize>,
    inline_key_size: Option<usize>,
    compact_variable_trees: bool,
}

impl BomWriter {
    pub fn new() -> Self {
        Self {
            blocks: vec![Vec::new()], // block 0 is always empty
            named_blocks: IndexMap::new(),
            inline_key_size: None,
            compact_variable_trees: false,
        }
    }

    /// Lay out variable-key trees (FACETKEYS, APPEARANCEKEYS) the way CoreUI
    /// 1000+ catalogs do: key size recorded as 0xFFFFFFFF and the leaf kept at
    /// exactly one block with no inline-key region.
    pub fn set_compact_variable_trees(&mut self, compact: bool) {
        self.compact_variable_trees = compact;
    }

    /// Set the fixed key size for subsequent `add_tree` calls. When set, the
    /// leaf node reserves `n_entries * key_size` zero bytes after the
    /// block_size padding, matching Apple's RENDITIONS leaf layout. Set back
    /// to `None` before adding variable-key trees (FACETKEYS, APPEARANCEKEYS).
    pub fn set_inline_key_size(&mut self, size: Option<usize>) {
        self.inline_key_size = size;
    }

    pub fn add_block(&mut self, data: Vec<u8>) -> usize {
        let idx = self.blocks.len();
        self.blocks.push(data);
        idx
    }

    pub fn add_named_block(&mut self, name: &str, data: Vec<u8>) -> usize {
        let idx = self.add_block(data);
        self.named_blocks.insert(name.to_string(), idx);
        idx
    }

    pub fn add_tree(
        &mut self,
        name: &str,
        entries: &[(Vec<u8>, Vec<u8>)],
        block_size: u32,
    ) -> usize {
        // Entry format: value_block_idx(4) + key_block_idx(4) = 8 bytes
        // Node header: isLeaf(2) + count(2) + forward(4) + backward(4) = 12 bytes
        let max_per_node = ((block_size as usize) - 12) / 8;

        // Fixed-key trees (RENDITIONS) use the caller-supplied key width.
        // Variable-key trees (FACETKEYS, APPEARANCEKEYS) still carry an
        // inline-key region whose width equals the longest key in the tree;
        // CUICatalog reads that region when enumerating facets/appearances.
        let compact = self.inline_key_size.is_none() && self.compact_variable_trees;
        let effective_key_size = self.inline_key_size.unwrap_or_else(|| {
            entries.iter().map(|(k, _)| k.len()).max().unwrap_or(0)
        });
        let (leaf_key_size, header_key_size) = if compact {
            (None, u32::MAX)
        } else {
            (Some(effective_key_size), effective_key_size as u32)
        };

        if entries.is_empty() {
            let mut node = Vec::with_capacity(block_size as usize);
            node.write_u16::<BigEndian>(1).unwrap();
            node.write_u16::<BigEndian>(0).unwrap();
            node.write_i32::<BigEndian>(0).unwrap();
            node.write_u32::<BigEndian>(0).unwrap();
            node.resize(block_size as usize, 0);
            let node_idx = self.add_block(node);
            let tree_hdr =
                build_tree_header(node_idx, block_size, 0, 0, header_key_size);
            let tree_idx = self.add_block(tree_hdr);
            self.named_blocks.insert(name.to_string(), tree_idx);
            return tree_idx;
        }

        let leaf_batches: Vec<&[(Vec<u8>, Vec<u8>)]> =
            entries.chunks(max_per_node).collect();

        if leaf_batches.len() == 1 {
            let node = self.build_leaf_node(
                leaf_batches[0],
                0,
                0,
                block_size,
                leaf_key_size,
            );
            let node_idx = self.add_block(node);
            let tree_hdr = build_tree_header(
                node_idx,
                block_size,
                entries.len() as u32,
                0,
                header_key_size,
            );
            let tree_idx = self.add_block(tree_hdr);
            self.named_blocks.insert(name.to_string(), tree_idx);
            return tree_idx;
        }

        // Reserve leaf indices before we know their contents.
        let leaf_indices: Vec<usize> = leaf_batches
            .iter()
            .map(|_| self.add_block(vec![0]))
            .collect();

        for (i, batch) in leaf_batches.iter().enumerate() {
            let fwd = if i + 1 < leaf_indices.len() {
                leaf_indices[i + 1] as u32
            } else {
                0
            };
            let bwd = if i > 0 { leaf_indices[i - 1] as u32 } else { 0 };
            let node = self.build_leaf_node(batch, fwd, bwd, block_size, leaf_key_size);
            self.blocks[leaf_indices[i]] = node;
        }

        let mut internal = Vec::new();
        internal.write_u16::<BigEndian>(0).unwrap(); // isLeaf = 0
        internal
            .write_u16::<BigEndian>((leaf_indices.len() - 1) as u16)
            .unwrap();
        internal.write_u32::<BigEndian>(0).unwrap();
        internal.write_u32::<BigEndian>(0).unwrap();
        internal
            .write_u32::<BigEndian>(leaf_indices[0] as u32)
            .unwrap();
        for i in 1..leaf_indices.len() {
            let first_key = leaf_batches[i][0].0.clone();
            let key_idx = self.add_block(first_key);
            internal.write_u32::<BigEndian>(key_idx as u32).unwrap();
            internal
                .write_u32::<BigEndian>(leaf_indices[i] as u32)
                .unwrap();
        }
        let internal_idx = self.add_block(internal);
        let tree_hdr = build_tree_header(
            internal_idx,
            block_size,
            entries.len() as u32,
            0,
            header_key_size,
        );
        let tree_idx = self.add_block(tree_hdr);
        self.named_blocks.insert(name.to_string(), tree_idx);
        tree_idx
    }


    fn build_leaf_node(
        &mut self,
        entries: &[(Vec<u8>, Vec<u8>)],
        forward: u32,
        backward: u32,
        block_size: u32,
        inline_key_size: Option<usize>,
    ) -> Vec<u8> {
        let mut node = Vec::new();
        node.write_u16::<BigEndian>(1).unwrap(); // isLeaf
        node.write_u16::<BigEndian>(entries.len() as u16).unwrap();
        node.write_u32::<BigEndian>(forward).unwrap();
        node.write_u32::<BigEndian>(backward).unwrap();
        let mut key_blob: Vec<u8> = Vec::new();
        for (key_data, value_data) in entries {
            let val_idx = self.add_block(value_data.clone());
            let key_idx = self.add_block(key_data.clone());
            node.write_u32::<BigEndian>(val_idx as u32).unwrap();
            node.write_u32::<BigEndian>(key_idx as u32).unwrap();
            if let Some(ks) = inline_key_size {
                key_blob.extend_from_slice(key_data);
                let pad = ks.saturating_sub(key_data.len());
                key_blob.resize(key_blob.len() + pad, 0);
            }
        }
        // For fixed-key trees (RENDITIONS) and variable-key trees whose keys
        // are padded to a common width, Apple inlines each key immediately
        // after the entries. A 4-byte zero gap separates the entry table from
        // the inline-key region.
        if !key_blob.is_empty() {
            node.extend_from_slice(&[0u8; 4]);
            node.extend_from_slice(&key_blob);
        }
        if (node.len() as u32) < block_size {
            node.resize(block_size as usize, 0);
        }
        // After the inline-key region the leaf is padded with zeros until
        // total length = block_size + n_entries * fixed_key_size.
        if let Some(ks) = inline_key_size {
            let target = block_size as usize + entries.len() * ks;
            if node.len() < target {
                node.resize(target, 0);
            }
        }
        node
    }

    pub fn add_raw_key_tree(
        &mut self,
        name: &str,
        entries: &[(u32, Vec<u8>)],
        block_size: u32,
    ) -> usize {
        if entries.is_empty() {
            let mut node = Vec::with_capacity(block_size as usize);
            node.write_u16::<BigEndian>(1).unwrap();
            node.write_u16::<BigEndian>(0).unwrap();
            node.write_u32::<BigEndian>(0).unwrap();
            node.write_u32::<BigEndian>(0).unwrap();
            node.resize(block_size as usize, 0);
            let node_idx = self.add_block(node);
            let tree_hdr = build_tree_header(node_idx, block_size, 0, 1, 0);
            let tree_idx = self.add_block(tree_hdr);
            self.named_blocks.insert(name.to_string(), tree_idx);
            return tree_idx;
        }

        let max_per_node = ((block_size as usize) - 12) / 8;
        let batches: Vec<&[(u32, Vec<u8>)]> =
            entries.chunks(max_per_node).collect();

        if batches.len() == 1 {
            let mut node = Vec::new();
            node.write_u16::<BigEndian>(1).unwrap();
            node.write_u16::<BigEndian>(entries.len() as u16).unwrap();
            node.write_u32::<BigEndian>(0).unwrap();
            node.write_u32::<BigEndian>(0).unwrap();
            for (raw_key, value_data) in entries {
                let val_idx = self.add_block(value_data.clone());
                node.write_u32::<BigEndian>(val_idx as u32).unwrap();
                node.write_u32::<BigEndian>(*raw_key).unwrap();
            }
            if (node.len() as u32) < block_size {
                node.resize(block_size as usize, 0);
            }
            let node_idx = self.add_block(node);
            let tree_hdr =
                build_tree_header(node_idx, block_size, entries.len() as u32, 1, 0);
            let tree_idx = self.add_block(tree_hdr);
            self.named_blocks.insert(name.to_string(), tree_idx);
            return tree_idx;
        }

        let leaf_indices: Vec<usize> =
            batches.iter().map(|_| self.add_block(vec![0])).collect();

        for (i, batch) in batches.iter().enumerate() {
            let fwd = if i + 1 < leaf_indices.len() {
                leaf_indices[i + 1] as u32
            } else {
                0
            };
            let bwd = if i > 0 { leaf_indices[i - 1] as u32 } else { 0 };
            let mut node = Vec::new();
            node.write_u16::<BigEndian>(1).unwrap();
            node.write_u16::<BigEndian>(batch.len() as u16).unwrap();
            node.write_u32::<BigEndian>(fwd).unwrap();
            node.write_u32::<BigEndian>(bwd).unwrap();
            for (raw_key, value_data) in *batch {
                let val_idx = self.add_block(value_data.clone());
                node.write_u32::<BigEndian>(val_idx as u32).unwrap();
                node.write_u32::<BigEndian>(*raw_key).unwrap();
            }
            if (node.len() as u32) < block_size {
                node.resize(block_size as usize, 0);
            }
            self.blocks[leaf_indices[i]] = node;
        }

        let mut internal = Vec::new();
        internal.write_u16::<BigEndian>(0).unwrap();
        internal
            .write_u16::<BigEndian>((leaf_indices.len() - 1) as u16)
            .unwrap();
        internal.write_u32::<BigEndian>(0).unwrap();
        internal.write_u32::<BigEndian>(0).unwrap();
        internal
            .write_u32::<BigEndian>(leaf_indices[0] as u32)
            .unwrap();
        for i in 1..leaf_indices.len() {
            let first_key = batches[i][0].0;
            internal.write_u32::<BigEndian>(first_key).unwrap();
            internal
                .write_u32::<BigEndian>(leaf_indices[i] as u32)
                .unwrap();
        }
        let internal_idx = self.add_block(internal);
        let tree_hdr =
            build_tree_header(internal_idx, block_size, entries.len() as u32, 1, 0);
        let tree_idx = self.add_block(tree_hdr);
        self.named_blocks.insert(name.to_string(), tree_idx);
        tree_idx
    }

    pub fn write<P: AsRef<Path>>(&self, path: P) -> std::io::Result<()> {
        let buf = self.to_bytes();
        std::fs::write(path, buf)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        const MIN_TABLE_SIZE: usize = 256;
        const FREELIST_SIZE: usize = 20;

        let header_size: u64 = 32;
        let mut current_offset: u64 = header_size;
        let num_used_blocks = self.blocks.len() as u32;

        let mut block_entries: Vec<(u32, u32)> = Vec::new();
        for (i, block_data) in self.blocks.iter().enumerate() {
            if i == 0 {
                block_entries.push((0, 0));
                continue;
            }
            if current_offset % 16 != 0 {
                current_offset += 16 - (current_offset % 16);
            }
            block_entries
                .push((current_offset as u32, block_data.len() as u32));
            current_offset += block_data.len() as u64;
        }
        while block_entries.len() < MIN_TABLE_SIZE {
            block_entries.push((0, 0));
        }

        let mut index_data = Vec::new();
        index_data
            .write_u32::<BigEndian>(block_entries.len() as u32)
            .unwrap();
        for (off, len) in &block_entries {
            index_data.write_u32::<BigEndian>(*off).unwrap();
            index_data.write_u32::<BigEndian>(*len).unwrap();
        }
        index_data.extend(std::iter::repeat(0u8).take(FREELIST_SIZE));

        let mut vars_data = Vec::new();
        vars_data
            .write_u32::<BigEndian>(self.named_blocks.len() as u32)
            .unwrap();
        for (name, block_idx) in &self.named_blocks {
            let name_bytes = name.as_bytes();
            vars_data.write_u32::<BigEndian>(*block_idx as u32).unwrap();
            vars_data.write_u8(name_bytes.len() as u8).unwrap();
            vars_data.extend_from_slice(name_bytes);
        }

        if current_offset % 16 != 0 {
            current_offset += 16 - (current_offset % 16);
        }
        let vars_offset = current_offset;
        let vars_length = vars_data.len() as u32;
        current_offset += vars_length as u64;

        if current_offset % 16 != 0 {
            current_offset += 16 - (current_offset % 16);
        }
        let index_offset = current_offset;
        let index_length = index_data.len() as u32;

        let mut out = std::io::Cursor::new(Vec::new());
        out.write_all(b"BOMStore").unwrap();
        out.write_u32::<BigEndian>(1).unwrap(); // version
        out.write_u32::<BigEndian>(num_used_blocks).unwrap();
        out.write_u32::<BigEndian>(index_offset as u32).unwrap();
        out.write_u32::<BigEndian>(index_length).unwrap();
        out.write_u32::<BigEndian>(vars_offset as u32).unwrap();
        out.write_u32::<BigEndian>(vars_length).unwrap();

        for (i, block_data) in self.blocks.iter().enumerate() {
            if i == 0 {
                continue;
            }
            let expected = block_entries[i].0 as u64;
            let pos = out.position();
            if pos < expected {
                let pad = (expected - pos) as usize;
                out.write_all(&vec![0u8; pad]).unwrap();
            }
            out.write_all(block_data).unwrap();
        }

        let pos = out.position();
        if pos < vars_offset {
            let pad = (vars_offset - pos) as usize;
            out.write_all(&vec![0u8; pad]).unwrap();
        }
        out.write_all(&vars_data).unwrap();

        let pos = out.position();
        if pos < index_offset {
            let pad = (index_offset - pos) as usize;
            out.write_all(&vec![0u8; pad]).unwrap();
        }
        out.write_all(&index_data).unwrap();

        let _ = out.seek(SeekFrom::Start(0));
        out.into_inner()
    }
}

impl Default for BomWriter {
    fn default() -> Self {
        Self::new()
    }
}

fn build_tree_header(
    root_idx: usize,
    block_size: u32,
    n_paths: u32,
    flag: u8,
    key_size: u32,
) -> Vec<u8> {
    // "tree"(4) + version(4) + root(4) + block_size(4) + n_paths(4) +
    // flag(1) + key_size(4) + pad(4).
    // key_size is the per-entry inline key length: the actual size for
    // fixed-key trees (RENDITIONS), 0xFFFFFFFF for variable-key trees
    // (FACETKEYS, APPEARANCEKEYS), or 0 for raw-key trees (BITMAPKEYS).
    let mut hdr = Vec::new();
    hdr.extend_from_slice(b"tree");
    hdr.write_u32::<BigEndian>(1).unwrap();
    hdr.write_u32::<BigEndian>(root_idx as u32).unwrap();
    hdr.write_u32::<BigEndian>(block_size).unwrap();
    hdr.write_u32::<BigEndian>(n_paths).unwrap();
    hdr.write_u8(flag).unwrap();
    hdr.write_u32::<BigEndian>(key_size).unwrap();
    hdr.write_u32::<BigEndian>(0).unwrap();
    hdr
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::ReadBytesExt;
    use std::io::Cursor;

    #[test]
    fn empty_bom() {
        let bom = BomWriter::new();
        let data = bom.to_bytes();
        assert_eq!(&data[..8], b"BOMStore");
        let mut c = Cursor::new(&data[8..]);
        assert_eq!(c.read_u32::<BigEndian>().unwrap(), 1); // version
        assert_eq!(c.read_u32::<BigEndian>().unwrap(), 1); // num blocks (just null)
    }

    #[test]
    fn single_named_block() {
        let mut bom = BomWriter::new();
        let idx = bom.add_named_block("HELLO", b"world data".to_vec());
        assert_eq!(idx, 1);
        let data = bom.to_bytes();
        assert_eq!(&data[..8], b"BOMStore");
        // The bytes "world data" must appear somewhere in the output
        assert!(data.windows(10).any(|w| w == b"world data"));
    }

    #[test]
    fn tree_single_leaf() {
        let mut bom = BomWriter::new();
        let entries = vec![(b"key1".to_vec(), b"val1".to_vec())];
        bom.add_tree("TREE", &entries, 4096);
        let data = bom.to_bytes();
        assert!(data.windows(4).any(|w| w == b"tree"));
        assert!(data.windows(4).any(|w| w == b"key1"));
        assert!(data.windows(4).any(|w| w == b"val1"));
    }

    #[test]
    fn raw_key_tree() {
        let mut bom = BomWriter::new();
        let entries: Vec<(u32, Vec<u8>)> =
            vec![(1, b"aaaa".to_vec()), (2, b"bbbb".to_vec())];
        bom.add_raw_key_tree("TBL", &entries, 1024);
        let data = bom.to_bytes();
        assert!(data.windows(4).any(|w| w == b"aaaa"));
        assert!(data.windows(4).any(|w| w == b"bbbb"));
    }

    #[test]
    fn named_block_ordering_preserved() {
        let mut bom = BomWriter::new();
        bom.add_named_block("FIRST", b"AAAA".to_vec());
        bom.add_named_block("SECOND", b"BBBB".to_vec());
        bom.add_named_block("THIRD", b"CCCC".to_vec());
        let data = bom.to_bytes();
        // Find the vars section by looking for the name bytes.
        let first = data
            .windows(5)
            .position(|w| w == b"FIRST")
            .expect("FIRST not found");
        let second = data
            .windows(6)
            .position(|w| w == b"SECOND")
            .expect("SECOND not found");
        let third = data
            .windows(5)
            .position(|w| w == b"THIRD")
            .expect("THIRD not found");
        assert!(first < second);
        assert!(second < third);
    }

    #[test]
    fn compact_variable_tree_records_unset_key_size_and_one_block_leaf() {
        let entries = vec![(b"alpha".to_vec(), vec![1u8]), (b"beta-longer".to_vec(), vec![2u8])];
        let mut compact = BomWriter::new();
        compact.set_compact_variable_trees(true);
        let tree = compact.add_tree("T", &entries, 4096);
        // Header: "tree", version, root, block size, path count, flag, key size.
        assert_eq!(&compact.blocks[tree][21..25], &u32::MAX.to_be_bytes());
        let root = u32::from_be_bytes(compact.blocks[tree][8..12].try_into().unwrap()) as usize;
        assert_eq!(compact.blocks[root].len(), 4096);

        let mut padded = BomWriter::new();
        let tree = padded.add_tree("T", &entries, 4096);
        assert_eq!(&padded.blocks[tree][21..25], &11u32.to_be_bytes());
        let root = u32::from_be_bytes(padded.blocks[tree][8..12].try_into().unwrap()) as usize;
        assert_eq!(padded.blocks[root].len(), 4096 + 2 * 11);
    }
}
