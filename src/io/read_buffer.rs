use std::io::ErrorKind;
use std::io::Read;
use std::io::Result;
use std::io::Write;

// This is pretty much std::io::ReadBuf except with the ability to "shift" the
// Unread data to the beginning of the buffer.
//
//   Read         Unread    Unused
//   XXXXXXXXXXXXYYYYYYYYYY-----------
//
//   consume(5)
//
//   XXXXXXXXXXXXXXXXXYYYYY-----------
//
//   shift_left()
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

impl Write for ReadBuffer {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let remaining = self.capacity - self.unused_start;
        let length = buf.len();
        if length > remaining {
            self.shift_left();
            if length > remaining {
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
        }
        let unused = self.get_unused_mut();
        unsafe {
            let dst_ptr = unused.as_mut_ptr();
            let src_ptr = buf.as_ptr();
            std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, length);
        }
        self.added_unused(length);
        Ok(length)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
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

    pub fn get_unused_mut(&mut self) -> &mut [u8] {
        &mut self.buffer[self.unused_start..]
    }

    pub fn added_unused(&mut self, num: usize) {
        self.unused_start += num;
    }

    pub fn prepend_unread(&mut self, data: &[u8]) {
        // TODO check math
        if self.unread_start <= data.len() {
            // Need to make room.  Ideally this doesn't happen often, because it can be expensive.
            let room_in_front = self.unread_start - 1; // TODO check math
            let room_in_back = self.capacity - self.unused_start; // TODO check math

            if room_in_front + room_in_back < data.len() {
                let extra_space_needed = data.len() - room_in_front - room_in_back; // TODO check math
                let mut new_buffer = vec![0; self.capacity + extra_space_needed];
                let size = self.unread();
                unsafe {
                    let dst_ptr = new_buffer.as_mut_ptr().add(data.len());
                    let src_ptr = data.as_ptr().add(self.unread_start);
                    std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, size);
                }
                self.buffer = new_buffer;
            } else {
                let shift_amount = data.len() - room_in_front;
                self.shift(shift_amount as isize);
            }
        }

        let start = self.unread_start - data.len(); // TODO verify this math
        unsafe {
            let dst_ptr = self.buffer.as_mut_ptr().add(start);
            let src_ptr = data.as_ptr();
            std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, data.len());
        }
        self.unread_start = start;
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
        if length > self.capacity {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                format!(
                    "Requested length {} is greater than capacity {}",
                    length, self.capacity
                ),
            ));
        }
        let need_to_read = length - current_unread;
        if need_to_read > self.unused() {
            self.shift_left();
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

    pub fn shift_left(&mut self) {
        self.shift(-(self.unread_start as isize));
    }

    // Maybe more accurately shift_unread
    fn shift(&mut self, amount: isize) {
        let size = self.unread();
        let new_unread_start = self.unread_start as isize + amount;
        debug_assert!(new_unread_start >= 0 && new_unread_start < self.capacity as isize);
        unsafe {
            let src = self.buffer.as_ptr().add(self.unread_start);
            let dst = self.buffer.as_mut_ptr().offset(new_unread_start);
            std::ptr::copy(src, dst, size);
        }
        self.unread_start = new_unread_start as usize;
        self.unused_start = self.unread_start + size;
    }

    pub fn unread(&self) -> usize {
        self.unused_start - self.unread_start
    }

    pub fn unused(&self) -> usize {
        self.capacity - self.unused_start
    }
}
