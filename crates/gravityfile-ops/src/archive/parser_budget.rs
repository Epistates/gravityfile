//! Cumulative parser I/O budget. This is not an allocator or process RSS limit.
use std::cell::Cell;
use std::io::{self, Read, Seek, SeekFrom};

pub(super) struct MetadataBudget {
    remaining: Cell<u64>,
    active: Cell<bool>,
    step_remaining: Cell<u64>,
    exhausted: Cell<bool>,
}

impl MetadataBudget {
    pub fn new(bytes: u64) -> Self {
        Self {
            remaining: Cell::new(bytes),
            active: Cell::new(true),
            step_remaining: Cell::new(bytes),
            exhausted: Cell::new(false),
        }
    }

    pub fn wrap<R>(&self, reader: R) -> Budgeted<'_, R> {
        Budgeted {
            reader,
            budget: self,
        }
    }

    pub fn exhausted(&self) -> bool {
        self.exhausted.get()
    }

    pub fn resume_metadata(&self, step_bytes: u64) {
        self.active.set(true);
        self.step_remaining.set(step_bytes);
    }

    pub fn finish_metadata(&self) {
        self.active.set(false);
    }
}

pub(super) struct Budgeted<'a, R> {
    reader: R,
    budget: &'a MetadataBudget,
}

impl<R: Read> Read for Budgeted<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !self.budget.active.get() {
            return self.reader.read(buffer);
        }
        let remaining = self
            .budget
            .remaining
            .get()
            .min(self.budget.step_remaining.get());
        if buffer.is_empty() {
            return Ok(0);
        }
        if remaining == 0 {
            self.budget.exhausted.set(true);
            return Err(io::Error::other("archive metadata read budget exhausted"));
        }
        let limit = buffer
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let read = self.reader.read(&mut buffer[..limit])?;
        self.budget
            .remaining
            .set(self.budget.remaining.get() - read as u64);
        self.budget
            .step_remaining
            .set(self.budget.step_remaining.get() - read as u64);
        Ok(read)
    }
}

impl<R: Seek> Seek for Budgeted<'_, R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        // Seeking never replenishes the budget, including parser retries.
        self.reader.seek(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn repeated_reads_and_seeks_share_one_budget() {
        let budget = MetadataBudget::new(5);
        let mut reader = budget.wrap(Cursor::new(b"abcdef"));
        let mut buffer = [0; 4];
        assert_eq!(reader.read(&mut buffer).unwrap(), 4);
        reader.rewind().unwrap();
        assert_eq!(reader.read(&mut buffer).unwrap(), 1);
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        assert!(reader.read(&mut buffer).is_err());
        assert!(budget.exhausted());
        reader.rewind().unwrap();
        assert!(reader.read(&mut buffer).is_err());
    }

    #[test]
    fn payload_reads_are_unrestricted_after_metadata_finishes() {
        let budget = MetadataBudget::new(1);
        let mut reader = budget.wrap(Cursor::new(b"abcdef"));
        let mut byte = [0];
        reader.read_exact(&mut byte).unwrap();
        assert!(!budget.exhausted());
        budget.finish_metadata();
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"bcdef");
    }
}
