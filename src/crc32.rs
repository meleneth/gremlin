#[derive(Debug, Clone)]
pub struct Hasher {
    state: u32,
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher {
    pub fn new() -> Self {
        Self { state: 0xffff_ffff }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            let idx = ((self.state ^ u32::from(*byte)) & 0xff) as usize;
            self.state = (self.state >> 8) ^ TABLE[idx];
        }
    }

    pub fn finalize(self) -> u32 {
        !self.state
    }
}

const TABLE: [u32; 256] = make_table();

const fn make_table() -> [u32; 256] {
    let mut table = [0_u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 {
                0xedb8_8320_u32 ^ (crc >> 1)
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_sfv_known_vector() {
        let mut hasher = Hasher::new();
        hasher.update(b"123456789");
        assert_eq!(format!("{:08x}", hasher.finalize()), "cbf43926");
    }
}
