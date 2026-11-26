//! A ZIP archive of a few files, deflated: what an auditor's data comes
//! in. Only what that needs: no directories, no ZIP64 (each file and the
//! archive under 4 GiB, fewer than 65,535 files).

use std::io::Write;

use chrono::{Datelike, NaiveDateTime, Timelike};
use erpcraft_kernel::{Error, Result};
use flate2::Compression;
use flate2::write::DeflateEncoder;

/// The archive of `files` (name, bytes), stamped `at`.
pub(crate) fn archive(files: &[(&str, &[u8])], at: NaiveDateTime) -> Result<Vec<u8>> {
    let time = ((at.hour() << 11) | (at.minute() << 5) | (at.second() / 2)) as u16;
    let date = (((at.year() - 1980).max(0) as u32) << 9 | (at.month() << 5) | at.day()) as u16;
    let mut out: Vec<u8> = vec![];
    let mut central: Vec<u8> = vec![];
    for (name, data) in files {
        let mut crc = flate2::Crc::new();
        crc.update(data);
        let mut encoder = DeflateEncoder::new(vec![], Compression::default());
        encoder.write_all(data).map_err(Error::internal)?;
        let packed = encoder.finish().map_err(Error::internal)?;
        let offset = u32::try_from(out.len()).map_err(Error::internal)?;
        let size = |n: usize| u32::try_from(n).map_err(Error::internal);
        let (packed_len, len) = (size(packed.len())?, size(data.len())?);
        let header = |signature: u32, central: bool| {
            let mut h: Vec<u8> = vec![];
            h.extend(signature.to_le_bytes());
            if central {
                h.extend(20u16.to_le_bytes()); // made by
            }
            h.extend(20u16.to_le_bytes()); // needed to extract
            h.extend(0x0800u16.to_le_bytes()); // UTF-8 names
            h.extend(8u16.to_le_bytes()); // deflate
            h.extend(time.to_le_bytes());
            h.extend(date.to_le_bytes());
            h.extend(crc.sum().to_le_bytes());
            h.extend(packed_len.to_le_bytes());
            h.extend(len.to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes()); // extra
            if central {
                h.extend(0u16.to_le_bytes()); // comment
                h.extend(0u16.to_le_bytes()); // disk
                h.extend(0u16.to_le_bytes()); // internal attributes
                h.extend(0u32.to_le_bytes()); // external attributes
                h.extend(offset.to_le_bytes());
            }
            h.extend(name.as_bytes());
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend(&packed);
        central.extend(header(0x0201_4b50, true));
    }
    let start = u32::try_from(out.len()).map_err(Error::internal)?;
    let count = files.len() as u16;
    let central_len = u32::try_from(central.len()).map_err(Error::internal)?;
    out.extend(central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(count.to_le_bytes());
    out.extend(count.to_le_bytes());
    out.extend(central_len.to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    Ok(out)
}
