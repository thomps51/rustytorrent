use write_to::{Length, ReadFrom, WriteTo};

#[derive(Debug, WriteTo, ReadFrom, Clone, Length)]
pub struct Header {
    type_ver_extension: u16, // WriteTo and ReadFrom have specialization issues with u8
    pub connection_id: u16,
    pub timestamp_microseconds: u32,
    pub timestamp_difference_microseconds: u32,
    pub wnd_size: u32,
    pub seq_nr: u16,
    pub ack_nr: u16,
}
#[derive(PartialEq, Debug, Clone, Copy)]
#[repr(u8)]
pub enum Type {
    StData = 0,
    StFin = 1,
    StState = 2,
    StReset = 3,
    StSyn = 4,
}

impl Header {
    pub const SIZE: usize = 20;

    pub fn get_type(&self) -> Type {
        let value = self.type_ver_extension >> 12;
        let result: Type = unsafe { std::mem::transmute(value as u8) };
        result
    }

    pub fn get_ver(&self) -> u8 {
        let result = (self.type_ver_extension & 0x0F00) >> 8;
        result as _
    }

    pub fn get_ext(&self) -> u8 {
        let result = self.type_ver_extension & 0x00FF;
        result as _
    }

    pub fn new(
        type_: Type,
        connection_id: u16,
        timestamp_microseconds: u32,
        timestamp_difference_microseconds: u32,
        wnd_size: u32,
        seq_nr: u16,
        ack_nr: u16,
    ) -> Self {
        let type_ver_extension: u16 = ((type_ as u16) << 12) | (1 << 8);
        println!("value: {type_ver_extension}");
        Header {
            type_ver_extension,
            connection_id,
            timestamp_microseconds,
            timestamp_difference_microseconds,
            wnd_size,
            seq_nr,
            ack_nr,
        }
    }
}

mod tests {
    #[test]
    fn test_header() {
        use super::{Header, Type};
        let header = Header {
            type_ver_extension: 16640, // Value from qbittorrent
            connection_id: 0,
            timestamp_microseconds: 0,
            timestamp_difference_microseconds: 0,
            wnd_size: 0,
            seq_nr: 0,
            ack_nr: 0,
        };
        assert_eq!(header.get_type(), Type::StSyn);
        assert_eq!(header.get_ver(), 1);
    }

    #[test]
    fn test_new_header() {
        use super::{Header, Type};
        let header = Header::new(Type::StSyn, 0, 0, 0, 0, 0, 0);
        assert_eq!(header.get_type(), Type::StSyn);
        assert_eq!(header.get_ver(), 1);
        assert_eq!(header.get_ext(), 0);
    }
}
