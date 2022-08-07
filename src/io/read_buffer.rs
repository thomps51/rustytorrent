use std::io::Read;
use std::io::Result;

//
//
//   Read         Unread    Unused
//   XXXXXXXXXXXXYYYYYYYYYY-----------
//
//   consume(5)
//
//   XXXXXXXXXXXXXXXXXYYYYY-----------
//
//   shift()
//
//   YYYYY----------------------------
pub struct ReadBuffer {
    unread_start: usize,
    unused_start: usize,
    capacity: usize,
    buffer: Vec<u8>,
}

impl Read for ReadBuffer {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let unread = self.get_unread();
        let length = std::cmp::min(buf.len(), unread.len());
        unsafe {
            let dst_ptr = buf.as_mut_ptr();
            let src_ptr = unread.as_ptr();
            std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, length);
        }
        self.consume(length);
        Ok(length)
    }
}

impl ReadBuffer {
    pub fn new(capacity: usize) -> Self {
        let mut buffer = Vec::new();
        buffer.resize(capacity, 0);
        ReadBuffer {
            unread_start: 0,
            unused_start: 0,
            capacity,
            buffer,
        }
    }

    pub fn clear(&mut self) {
        self.unread_start = 0;
        self.unused_start = 0;
    }

    pub fn consume(&mut self, num: usize) {
        self.unread_start += num;
    }

    pub fn get_unread(&self) -> &[u8] {
        &self.buffer[self.unread_start..self.unused_start]
    }

    pub fn read_from<T: std::io::Read>(&mut self, reader: &mut T) -> Result<usize> {
        let read = reader.read(&mut self.buffer[self.unused_start..])?;
        self.unused_start += read;
        Ok(read)
    }

    // If there is already length in the buffer, returns true.  Otherwise attempts to read from
    // reader into self, and returns whether there are length bytes in the buffer after reading.
    //
    // If an error other than std::io::ErrorKind::WouldBlock is produced when reading, it is
    // returned.
    pub fn read_at_least_from<T: std::io::Read>(
        &mut self,
        length: usize,
        reader: &mut T,
    ) -> Result<bool> {
        let current_unread = self.unread();
        if current_unread >= length {
            return Ok(true);
        }
        let need_to_read = length - current_unread;
        if need_to_read > self.unused() {
            self.shift();
        }
        let read = match self.read_from(reader) {
            Ok(l) => l,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    return Ok(false);
                }
                return Err(error);
            }
        };
        Ok(read >= need_to_read)
    }

    pub fn shift(&mut self) {
        let size = self.unread();
        unsafe {
            let src = self.buffer.as_ptr().offset(self.unread_start as isize);
            let dst = self.buffer.as_mut_ptr();
            std::ptr::copy(src, dst, size);
        }
        self.unread_start = 0;
        self.unused_start = size;
    }

    pub fn unread(&self) -> usize {
        self.unused_start - self.unread_start
    }

    pub fn unused(&self) -> usize {
        self.capacity - self.unused_start
    }
}
