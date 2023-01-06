use super::Data;
use super::DataKind;
use super::Dictionary;
use super::List;

use std::io::Write;
use std::vec::Vec;

pub trait Encode: ToOwned {
    fn bencode(&self) -> Vec<u8>;

    fn bencode_to<T: Write>(&self, writer: &mut T) -> std::io::Result<()>;
}

impl Encode for &[u8] {
    fn bencode(&self) -> Vec<u8> {
        let mut result = Vec::new();
        self.bencode_to(&mut result).unwrap();
        result
    }

    fn bencode_to<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        write!(writer, "{}:", self.len())?;
        writer.write_all(self)?;
        Ok(())
    }
}

impl Encode for Data {
    fn bencode(&self) -> Vec<u8> {
        let mut result = Vec::new();
        self.bencode_to(&mut result).unwrap();
        result
    }

    fn bencode_to<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        (&self[..]).bencode_to(writer)
    }
}

impl Encode for String {
    fn bencode(&self) -> Vec<u8> {
        self.as_bytes().bencode()
    }

    fn bencode_to<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.as_str().bencode_to(writer)
    }
}

impl Encode for &str {
    fn bencode(&self) -> Vec<u8> {
        self.as_bytes().bencode()
    }

    fn bencode_to<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        self.as_bytes().bencode_to(writer)
    }
}

impl Encode for i64 {
    fn bencode(&self) -> Vec<u8> {
        format!("i{self}e").into_bytes()
    }

    fn bencode_to<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        write!(writer, "i{self}e")?;
        Ok(())
    }
}

impl Encode for List {
    fn bencode(&self) -> Vec<u8> {
        let mut result = Vec::new();
        self.bencode_to(&mut result).unwrap();
        result
    }

    fn bencode_to<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&[b'l'])?;
        for element in self {
            element.bencode_to(writer)?;
        }
        writer.write_all(&[b'e'])?;
        Ok(())
    }
}

impl Encode for Dictionary {
    fn bencode(&self) -> Vec<u8> {
        let mut result = Vec::new();
        self.bencode_to(&mut result).unwrap();
        result
    }

    fn bencode_to<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        writer.write_all(&[b'd'])?;
        for (key, value) in self.iter() {
            key.bencode_to(writer)?;
            value.bencode_to(writer)?;
        }
        writer.write_all(&[b'e'])?;
        Ok(())
    }
}

// Is there some way to make this nicer?
impl Encode for DataKind {
    fn bencode(&self) -> Vec<u8> {
        let mut result = Vec::new();
        self.bencode_to(&mut result).unwrap();
        result
    }

    fn bencode_to<T: Write>(&self, writer: &mut T) -> std::io::Result<()> {
        match self {
            DataKind::Data(value) => value.bencode_to(writer),
            DataKind::Integer(value) => value.bencode_to(writer),
            DataKind::List(value) => value.bencode_to(writer),
            DataKind::Dictionary(value) => value.bencode_to(writer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DataKind;
    use super::Dictionary;
    use crate::bencoding::encoder::Encode;

    #[test]
    fn encode_integer_test() {
        let expected = "i-3e".as_bytes();
        let result = (-3_i64).bencode();
        assert_eq!(expected, result.as_slice());
    }
    #[test]
    fn encode_string_test() {
        let expected = "3:cow";
        let result = "cow".bencode();
        assert_eq!(expected, std::str::from_utf8(result.as_slice()).unwrap());
    }
    #[test]
    fn encode_list_test() {
        let expected = "l4:spam4:eggse";
        let result = vec![DataKind::Data("spam".into()), DataKind::Data("eggs".into())].bencode();
        assert_eq!(expected, std::str::from_utf8(result.as_slice()).unwrap());
    }
    #[test]
    fn encode_list_of_list_test() {
        let expected = "ll4:spam4:eggse4:spam4:eggse".as_bytes();
        let result = vec![
            DataKind::List(vec![
                DataKind::Data("spam".into()),
                DataKind::Data("eggs".into()),
            ]),
            DataKind::Data("spam".into()),
            DataKind::Data("eggs".into()),
        ]
        .bencode();
        assert_eq!(expected, result.as_slice());
    }
    #[test]
    fn encode_dictionary_test() {
        let expected = "d3:cow3:moo4:spam4:eggse";
        let mut map = Dictionary::new();
        map.insert("cow".into(), DataKind::Data("moo".into()));
        map.insert("spam".into(), DataKind::Data("eggs".into()));
        let result = DataKind::Dictionary(map).bencode();
        assert_eq!(expected, std::str::from_utf8(result.as_slice()).unwrap());
    }
    #[test]
    fn encode_dictionary_with_dictionary_test() {
        let expected = "d3:cow3:moo4:spamd3:cow3:mooee";
        let mut map = Dictionary::new();
        map.insert("cow".into(), DataKind::Data("moo".into()));
        map.insert("spam".into(), DataKind::Dictionary(map.clone()));
        let dictionary = DataKind::Dictionary(map);
        let result = dictionary.bencode();
        assert_eq!(expected, std::str::from_utf8(result.as_slice()).unwrap());
    }
}
