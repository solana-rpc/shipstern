//! Fixture capture and replay for deterministic integration tests.
//!
//! Records raw `SubscribeUpdate` protobuf messages from the geyser stream
//! to a length-delimited file, then replays them in tests.
//!
//! File format: `[4 bytes: u32 BE message length][N bytes: SubscribeUpdate protobuf]...`

use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Write},
    path::Path,
};

use prost::Message;
use yellowstone_grpc_proto::geyser::{subscribe_update::UpdateOneof, SubscribeUpdate};

/// Writes `SubscribeUpdate` messages to a length-delimited protobuf file.
///
/// Used at runtime during `--capture-fixtures` mode. Tracks the number of
/// `BlockMeta` messages seen to know when the target slot count is reached.
pub struct FixtureWriter {
    writer: BufWriter<File>,
    target_slots: usize,
    block_meta_count: usize,
}

impl FixtureWriter {
    pub fn new(path: &Path, target_slots: usize) -> io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            target_slots,
            block_meta_count: 0,
        })
    }

    /// Encode and write a single `SubscribeUpdate` to the fixture file.
    ///
    /// Returns `true` if capture should continue, `false` when the target
    /// number of BlockMeta messages has been reached.
    /// Flush any buffered data to disk. Call this when the stream ends
    /// before the target slot count is reached to avoid losing trailing messages.
    pub fn finish(mut self) -> io::Result<()> { self.writer.flush() }

    pub fn write(&mut self, update: &SubscribeUpdate) -> io::Result<bool> {
        let bytes = update.encode_to_vec();
        let len: u32 = bytes.len().try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Message too large for u32 length prefix: {} bytes",
                    bytes.len()
                ),
            )
        })?;
        self.writer.write_all(&len.to_be_bytes())?;
        self.writer.write_all(&bytes)?;

        if matches!(update.update_oneof, Some(UpdateOneof::BlockMeta(_))) {
            self.block_meta_count += 1;
            if self.block_meta_count >= self.target_slots {
                self.writer.flush()?;
                return Ok(false);
            }
        }

        Ok(true)
    }
}

/// Reads `SubscribeUpdate` messages from a length-delimited protobuf file.
///
/// Implements `Iterator` for convenient replay in tests.
pub struct FixtureReader {
    reader: BufReader<File>,
}

impl FixtureReader {
    pub fn new(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self {
            reader: BufReader::new(file),
        })
    }
}

impl Iterator for FixtureReader {
    type Item = SubscribeUpdate;

    fn next(&mut self) -> Option<Self::Item> {
        let mut len_buf = [0u8; 4];
        match self.reader.read_exact(&mut len_buf) {
            Ok(()) => {},
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return None,
            Err(e) => {
                tracing::error!(?e, "FixtureReader: I/O error reading length prefix");
                return None;
            },
        }

        let len = u32::from_be_bytes(len_buf) as usize;
        let mut msg_buf = vec![0u8; len];
        if let Err(e) = self.reader.read_exact(&mut msg_buf) {
            tracing::error!(?e, len, "FixtureReader: I/O error reading message body");
            return None;
        }

        match SubscribeUpdate::decode(&msg_buf[..]) {
            Ok(update) => Some(update),
            Err(e) => {
                tracing::error!(?e, len, "FixtureReader: protobuf decode failed");
                None
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_update(slot: u64) -> SubscribeUpdate {
        SubscribeUpdate {
            filters: vec![],
            created_at: None,
            update_oneof: Some(UpdateOneof::Slot(
                yellowstone_grpc_proto::geyser::SubscribeUpdateSlot {
                    slot,
                    parent: Some(slot.saturating_sub(1)),
                    status: 0,
                    dead_error: None,
                    bank_id: None,
                },
            )),
        }
    }

    #[test]
    fn writer_reader_roundtrip() {
        let dir = std::env::temp_dir().join("block-coordinator-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.bin");

        let updates: Vec<_> = (100..105).map(make_test_update).collect();

        // Write
        let mut writer = FixtureWriter::new(&path, usize::MAX).unwrap();
        for update in &updates {
            writer.write(update).unwrap();
        }
        writer.finish().unwrap();

        // Read back
        let read_back: Vec<_> = FixtureReader::new(&path).unwrap().collect();
        assert_eq!(read_back.len(), updates.len());
        for (original, decoded) in updates.iter().zip(read_back.iter()) {
            assert_eq!(original.update_oneof, decoded.update_oneof);
        }

        std::fs::remove_file(&path).ok();
    }
}
