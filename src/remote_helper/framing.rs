#[cfg(test)]
use std::io::Read;
use std::io::Write;

pub fn write_helper_frame(writer: &mut impl Write, helper_bytes: &[u8]) -> std::io::Result<()> {
    writer.write_all(&(helper_bytes.len() as u64).to_be_bytes())?;
    writer.write_all(helper_bytes)?;
    Ok(())
}

#[cfg(test)]
pub fn read_helper_frame(reader: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut header = [0_u8; 8];
    reader.read_exact(&mut header)?;
    let len = u64::from_be_bytes(header);
    let mut bytes = vec![0_u8; len as usize];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frames_helper_length_as_u64_big_endian() {
        let mut out = Vec::new();
        write_helper_frame(&mut out, b"ELF").unwrap();

        assert_eq!(&out[..8], &3_u64.to_be_bytes());
        assert_eq!(&out[8..], b"ELF");
        assert_eq!(read_helper_frame(&mut Cursor::new(out)).unwrap(), b"ELF");
    }

    #[test]
    fn truncated_frame_is_an_error() {
        let mut input = Vec::new();
        input.extend_from_slice(&4_u64.to_be_bytes());
        input.extend_from_slice(b"EL");

        let err = read_helper_frame(&mut Cursor::new(input)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}
