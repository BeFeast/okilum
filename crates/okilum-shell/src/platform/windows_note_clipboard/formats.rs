//! Bounded decoding of Windows clipboard representations, independent of Win32.
use gpui_component::input::clipboard::ClipboardReadError as Error;

pub(crate) const MAX_IMAGE: usize = crate::note_paste::offer::MAX_IMAGE_BYTES;

pub(crate) fn unicode(bytes: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    if !bytes.len().is_multiple_of(2) || bytes.len() > limit * 2 + 64 {
        return Err(Error::TooLarge);
    }
    let mut words = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair));
    let mut terminated = false;
    let words: Vec<_> = words
        .by_ref()
        .take_while(|word| {
            if *word == 0 {
                terminated = true;
                false
            } else {
                true
            }
        })
        .collect();
    if !terminated {
        return Err(Error::Io);
    }
    let text = String::from_utf16(&words)
        .map_err(|_| Error::Io)?
        .replace("\r\n", "\n");
    if text.len() > limit {
        return Err(Error::TooLarge);
    }
    Ok(text.into_bytes())
}

/// Wrap a packed DIB in a BMP file header. Validate arithmetic and palette/mask
/// layout before handing it to the existing image decoder; never touch pixels.
pub(crate) fn dib_to_bmp(dib: &[u8]) -> Result<Vec<u8>, Error> {
    if dib.len() > MAX_IMAGE - 14 {
        return Err(Error::TooLarge);
    }
    let u32_at = |at: usize| -> Result<u32, Error> {
        Ok(u32::from_le_bytes(
            dib.get(at..at + 4).ok_or(Error::Io)?.try_into().unwrap(),
        ))
    };
    let header = u32_at(0)? as usize;
    if ![40, 52, 56, 108, 124].contains(&header) || header > dib.len() {
        return Err(Error::Io);
    }
    let bits = u16::from_le_bytes(dib.get(14..16).ok_or(Error::Io)?.try_into().unwrap());
    if ![1, 4, 8, 16, 24, 32].contains(&bits) {
        return Err(Error::Io);
    }
    let compression = u32_at(16)?;
    // JPEG/PNG-compressed DIB and RLE are outside the packed RGB clipboard path.
    if ![0, 3, 6].contains(&compression) {
        return Err(Error::Io);
    }
    let colors = u32_at(32)? as usize;
    let colors = if colors == 0 && bits <= 8 {
        1usize << bits
    } else {
        colors
    };
    if colors > 256 {
        return Err(Error::Io);
    }
    let masks = if header == 40 {
        match compression {
            3 => 12,
            6 => 16,
            _ => 0,
        }
    } else {
        0
    };
    let offset = header
        .checked_add(masks)
        .and_then(|v| v.checked_add(colors * 4))
        .ok_or(Error::Io)?;
    let width = i32::from_le_bytes(dib[4..8].try_into().unwrap());
    let height = i32::from_le_bytes(dib[8..12].try_into().unwrap());
    let planes = u16::from_le_bytes(dib[12..14].try_into().unwrap());
    if width <= 0 || height == 0 || height == i32::MIN || planes != 1 {
        return Err(Error::Io);
    }
    let stride = (width as usize)
        .checked_mul(bits as usize)
        .and_then(|v| v.checked_add(31))
        .map(|v| v / 32 * 4)
        .ok_or(Error::Io)?;
    let end = stride
        .checked_mul(height.unsigned_abs() as usize)
        .and_then(|v| offset.checked_add(v))
        .ok_or(Error::Io)?;
    if end > dib.len() {
        return Err(Error::Io);
    }
    let mut bmp = Vec::with_capacity(dib.len() + 14);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&((dib.len() + 14) as u32).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&((offset + 14) as u32).to_le_bytes());
    bmp.extend_from_slice(dib);
    Ok(bmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_stops_at_nul_normalizes_crlf_and_rejects_bad_surrogates() {
        let bytes: Vec<_> = "שלום\r\n🧠\0ignored"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(unicode(&bytes, 100).unwrap(), "שלום\n🧠".as_bytes());
        assert_eq!(unicode(&[0, 0xd8, 0, 0], 100), Err(Error::Io));
        assert_eq!(unicode(&[65, 0], 100), Err(Error::Io));
        assert_eq!(unicode(&bytes, 2), Err(Error::TooLarge));
    }
    fn dib(header: usize, bits: u16, compression: u32, extra: usize) -> Vec<u8> {
        let mut dib = vec![0; header + extra + 4];
        dib[..4].copy_from_slice(&(header as u32).to_le_bytes());
        dib[4..8].copy_from_slice(&1i32.to_le_bytes());
        dib[8..12].copy_from_slice(&(-1i32).to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&bits.to_le_bytes());
        dib[16..20].copy_from_slice(&compression.to_le_bytes());
        dib
    }
    #[test]
    fn dib_rgb_palette_bitfields_and_v5_have_correct_pixel_offsets() {
        for (header, bits, compression, extra) in [
            (40, 24, 0, 0),
            (40, 8, 0, 1024),
            (40, 32, 3, 12),
            (124, 32, 3, 0),
        ] {
            let dib = dib(header, bits, compression, extra);
            let bmp = dib_to_bmp(&dib).unwrap();
            assert_eq!(&bmp[..2], b"BM");
            assert_eq!(
                u32::from_le_bytes(bmp[10..14].try_into().unwrap()) as usize,
                header + extra + 14
            );
            assert_eq!(&bmp[14..], dib);
            assert!(dib_to_bmp(&dib[..dib.len() - 1]).is_err());
        }
        assert!(dib_to_bmp(&[0; 40]).is_err());
        assert!(dib_to_bmp(&dib(40, 32, 5, 0)).is_err());
    }
}
