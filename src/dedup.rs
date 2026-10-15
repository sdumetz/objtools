//! Exact corner deduplication for one primitive at a time, in mapped workspace files, so that
//! primitive size never costs heap. A table indexed by source position holds the first
//! `(vt, vn)` seen for it; further combinations at that position (UV or normal seams) go to
//! overflow records chained from the table entry.
use crate::{import::Res, obj::Corner};
use memmap2::MmapMut;
use std::{fs::OpenOptions, path::Path};
/// generation u32, output index u32, chain head u32 (record + 1, 0 = none), vt u64, vn u64.
const ENTRY: usize = 28;
/// vt u64, vn u64, output index u32, next u32 (record + 1, 0 = none).
const RECORD: usize = 24;
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
fn sparse(path: &Path, len: u64) -> Res<MmapMut> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?;
    file.set_len(len)?;
    // SAFETY: the file was just created in the private workspace and is only accessed through
    // this mapping. It is sparse: a full disk surfaces as SIGBUS rather than an error.
    Ok(unsafe { MmapMut::map_mut(&file)? })
}
pub struct Dedup {
    table: MmapMut,
    records: MmapMut,
    used: usize,
    generation: u32,
}
impl Dedup {
    /// `corners` bounds the corners of any one primitive, and so its overflow records.
    pub fn create(dir: &Path, positions: u64, corners: u64) -> Res<Self> {
        let size = |n: u64, width: usize| {
            n.checked_mul(width as u64)
                .ok_or("Deduplication table size overflow")
        };
        Ok(Self {
            table: sparse(&dir.join("dedup-table"), size(positions, ENTRY)?)?,
            records: sparse(&dir.join("dedup-seams"), size(corners, RECORD)?)?,
            used: 0,
            generation: 0,
        })
    }
    /// Start a primitive. Entries of earlier generations read as empty, so nothing is cleared.
    pub fn reset(&mut self) -> Res<()> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("More than 2^32 primitives")?;
        self.used = 0;
        Ok(())
    }
    /// The output index of an identical earlier corner of this primitive, or `None` after
    /// recording that the corner becomes vertex `next`. `corner.v` must be a valid position.
    pub fn find_or_insert(&mut self, corner: Corner, next: u32) -> Res<Option<u32>> {
        let at = usize::try_from(corner.v - 1)? * ENTRY;
        let entry = &mut self.table[at..at + ENTRY];
        if u32_at(entry, 0) != self.generation {
            entry[0..4].copy_from_slice(&self.generation.to_le_bytes());
            entry[4..8].copy_from_slice(&next.to_le_bytes());
            entry[8..12].fill(0);
            entry[12..20].copy_from_slice(&corner.vt.to_le_bytes());
            entry[20..28].copy_from_slice(&corner.vn.to_le_bytes());
            return Ok(None);
        }
        if u64_at(entry, 12) == corner.vt && u64_at(entry, 20) == corner.vn {
            return Ok(Some(u32_at(entry, 4)));
        }
        let head = u32_at(entry, 8);
        let mut link = head;
        while link != 0 {
            let r = &self.records[(link as usize - 1) * RECORD..][..RECORD];
            if u64_at(r, 0) == corner.vt && u64_at(r, 8) == corner.vn {
                return Ok(Some(u32_at(r, 16)));
            }
            link = u32_at(r, 20);
        }
        let id = self.used;
        let r = self
            .records
            .get_mut(id * RECORD..(id + 1) * RECORD)
            .ok_or("Seam records exceed the primitive's corner count")?;
        r[0..8].copy_from_slice(&corner.vt.to_le_bytes());
        r[8..16].copy_from_slice(&corner.vn.to_le_bytes());
        r[16..20].copy_from_slice(&next.to_le_bytes());
        r[20..24].copy_from_slice(&head.to_le_bytes());
        self.used += 1;
        let link = u32::try_from(self.used)?;
        self.table[at + 8..at + 12].copy_from_slice(&link.to_le_bytes());
        Ok(None)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_seams_and_generations() {
        let dir = std::env::temp_dir().join(format!("objtools-dedup-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let mut d = Dedup::create(&dir, 4, 16).unwrap();
        let c = |v, vt, vn| Corner { v, vt, vn };
        d.reset().unwrap();
        assert_eq!(d.find_or_insert(c(2, 1, 0), 0).unwrap(), None);
        assert_eq!(d.find_or_insert(c(2, 1, 0), 1).unwrap(), Some(0));
        // Two seams at the same position, each found again exactly.
        assert_eq!(d.find_or_insert(c(2, 5, 0), 1).unwrap(), None);
        assert_eq!(d.find_or_insert(c(2, 1, 7), 2).unwrap(), None);
        assert_eq!(d.find_or_insert(c(2, 5, 0), 3).unwrap(), Some(1));
        assert_eq!(d.find_or_insert(c(2, 1, 7), 3).unwrap(), Some(2));
        assert_eq!(d.find_or_insert(c(2, 1, 0), 3).unwrap(), Some(0));
        assert_eq!(d.find_or_insert(c(4, 1, 0), 3).unwrap(), None);
        // A new primitive forgets everything.
        d.reset().unwrap();
        assert_eq!(d.find_or_insert(c(2, 5, 0), 0).unwrap(), None);
        assert_eq!(d.find_or_insert(c(2, 1, 0), 1).unwrap(), None);
        assert_eq!(d.find_or_insert(c(2, 5, 0), 2).unwrap(), Some(0));
        drop(d);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
