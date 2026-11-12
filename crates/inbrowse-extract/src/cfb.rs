//! Which application wrote an OLE compound file (the container of legacy
//! `.xls`, `.doc` and `.ppt`, and of password-protected OOXML), judged from the
//! stream names in its directory. Called by `detect.rs` before any format
//! reader runs; calamine's own CFB reader is private and cannot be asked this.
//!
//! The input is untrusted: every read is bounds-checked against the file,
//! chains are followed at most once per sector in the file, and the number of
//! directory entries visited is capped by `Limits::max_archive_entries`.

use crate::{ExtractError, Limits};

/// What a compound file's streams say it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Application {
    /// A `Workbook` (BIFF8) or `Book` (BIFF5) stream: a legacy `.xls`.
    Excel,
    /// A `WordDocument` stream: a legacy `.doc`.
    Word,
    /// A `PowerPoint Document` stream: a legacy `.ppt`.
    PowerPoint,
    /// An `EncryptedPackage` stream: an OOXML file with a password.
    EncryptedPackage,
    /// Something else stored as a compound file (`.msg`, `.msi`, ...).
    Other,
}

const HEADER_LEN: usize = 512;
const SECTOR_SHIFT_AT: usize = 30;
const FIRST_DIRECTORY_SECTOR_AT: usize = 48;
const FIRST_DIFAT_SECTOR_AT: usize = 68;
const DIFAT_SECTOR_COUNT_AT: usize = 72;
const HEADER_DIFAT_AT: usize = 76;
const HEADER_DIFAT_ENTRIES: usize = 109;
const DIRECTORY_ENTRY_LEN: usize = 128;
const STREAM_OBJECT: u8 = 2;
const END_OF_CHAIN: u32 = 0xFFFF_FFFE;
/// Sector numbers above this are markers (free, end of chain, FAT, DIFAT).
const MAX_REGULAR_SECTOR: u32 = 0xFFFF_FFFA;

fn malformed(why: &str) -> ExtractError {
    ExtractError::Malformed(format!("OLE compound file: {why}"))
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// A compound file's sectors and the allocation table that links them.
struct Container<'a> {
    bytes: &'a [u8],
    sector_len: usize,
    /// Sector numbers of the FAT sectors, in table order.
    fat: Vec<u32>,
    /// How many sectors the file can hold; no chain may be longer.
    sector_count: usize,
}

impl<'a> Container<'a> {
    /// Parse the header and gather the FAT sector list, including DIFAT
    /// sectors when the file has more than 109 FAT sectors.
    fn open(bytes: &'a [u8]) -> Result<Self, ExtractError> {
        if bytes.len() < HEADER_LEN {
            return Err(malformed("the header is cut off"));
        }
        let shift =
            u16_at(bytes, SECTOR_SHIFT_AT).ok_or_else(|| malformed("the header is cut off"))?;
        // Versions 3 and 4 allow 512 and 4096 byte sectors, nothing else.
        if shift != 9 && shift != 12 {
            return Err(malformed("unsupported sector size"));
        }
        let sector_len = 1usize << shift;
        let mut container = Self {
            bytes,
            sector_len,
            fat: Vec::new(),
            sector_count: (bytes.len() / sector_len).max(1),
        };
        for i in 0..HEADER_DIFAT_ENTRIES {
            let id = u32_at(bytes, HEADER_DIFAT_AT + i * 4)
                .ok_or_else(|| malformed("the header is cut off"))?;
            if id <= MAX_REGULAR_SECTOR {
                container.fat.push(id);
            }
        }
        let mut difat = u32_at(bytes, FIRST_DIFAT_SECTOR_AT)
            .ok_or_else(|| malformed("the header is cut off"))?;
        let difat_count = u32_at(bytes, DIFAT_SECTOR_COUNT_AT)
            .ok_or_else(|| malformed("the header is cut off"))?;
        let mut followed = 0usize;
        while difat <= MAX_REGULAR_SECTOR && followed < difat_count as usize {
            followed += 1;
            if followed > container.sector_count {
                return Err(malformed("the DIFAT chain loops"));
            }
            let sector = container.sector(difat)?;
            let entries = sector_len / 4 - 1;
            for i in 0..entries {
                let id =
                    u32_at(sector, i * 4).ok_or_else(|| malformed("a DIFAT sector is cut off"))?;
                if id <= MAX_REGULAR_SECTOR {
                    container.fat.push(id);
                }
            }
            difat = u32_at(sector, entries * 4)
                .ok_or_else(|| malformed("a DIFAT sector is cut off"))?;
        }
        if container.fat.len() > container.sector_count {
            return Err(malformed("more FAT sectors than the file can hold"));
        }
        Ok(container)
    }

    /// The bytes of sector `id`; sector 0 follows the header.
    fn sector(&self, id: u32) -> Result<&'a [u8], ExtractError> {
        let start = (id as usize)
            .checked_add(1)
            .and_then(|n| n.checked_mul(self.sector_len))
            .ok_or_else(|| malformed("a sector number is out of range"))?;
        let end = start
            .checked_add(self.sector_len)
            .ok_or_else(|| malformed("a sector number is out of range"))?;
        self.bytes
            .get(start..end)
            .ok_or_else(|| malformed("a sector lies past the end of the file"))
    }

    /// The sector after `id` in its chain, from the FAT.
    fn next(&self, id: u32) -> Result<u32, ExtractError> {
        let per_sector = self.sector_len / 4;
        let table = self
            .fat
            .get(id as usize / per_sector)
            .ok_or_else(|| malformed("a sector has no FAT entry"))?;
        let sector = self.sector(*table)?;
        u32_at(sector, (id as usize % per_sector) * 4)
            .ok_or_else(|| malformed("a FAT sector is cut off"))
    }
}

/// Name the application behind the compound file in `bytes`, which must start
/// with the OLE signature. A directory that cannot be followed is
/// [`ExtractError::Malformed`]; more entries than `limits.max_archive_entries`
/// is [`ExtractError::TooLarge`].
pub(crate) fn identify(bytes: &[u8], limits: &Limits) -> Result<Application, ExtractError> {
    let container = Container::open(bytes)?;
    let mut sector = u32_at(bytes, FIRST_DIRECTORY_SECTOR_AT)
        .ok_or_else(|| malformed("the header is cut off"))?;
    let (mut excel, mut word, mut powerpoint, mut encrypted) = (false, false, false, false);
    let mut entries = 0usize;
    let mut followed = 0usize;
    while sector <= MAX_REGULAR_SECTOR {
        followed += 1;
        if followed > container.sector_count {
            return Err(malformed("the directory chain loops"));
        }
        for entry in container.sector(sector)?.chunks_exact(DIRECTORY_ENTRY_LEN) {
            entries += 1;
            if entries > limits.max_archive_entries {
                return Err(ExtractError::TooLarge(format!(
                    "OLE directory lists more than {} entries",
                    limits.max_archive_entries
                )));
            }
            if entry[66] != STREAM_OBJECT {
                continue;
            }
            // The stored length counts the terminating NUL, in bytes, up to 64.
            let name_len = u16_at(entry, 64).map_or(0, |len| usize::from(len).min(64));
            let units: Vec<u16> = entry[..name_len.saturating_sub(2)]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            match String::from_utf16_lossy(&units).as_str() {
                "Workbook" | "Book" => excel = true,
                "WordDocument" => word = true,
                "PowerPoint Document" => powerpoint = true,
                "EncryptedPackage" => encrypted = true,
                _ => {}
            }
        }
        sector = container.next(sector)?;
    }
    if sector != END_OF_CHAIN {
        return Err(malformed(
            "the directory chain ends in a marker that is not end of chain",
        ));
    }
    // An encrypted OOXML file also carries streams of its own, so it wins.
    Ok(if encrypted {
        Application::EncryptedPackage
    } else if word {
        Application::Word
    } else if powerpoint {
        Application::PowerPoint
    } else if excel {
        Application::Excel
    } else {
        Application::Other
    })
}
