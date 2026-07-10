#[path = "../crc32.rs"]
mod crc32;
#[path = "../remote_helper/mod.rs"]
mod remote_helper;

use std::io::{BufReader, BufWriter};

fn main() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    remote_helper::session::run_session(BufReader::new(stdin.lock()), BufWriter::new(stdout.lock()))
}
