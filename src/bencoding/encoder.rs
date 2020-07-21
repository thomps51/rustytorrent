use super::Data;
use super::DataKind;
use super::Dictionary;
use super::List;

use std::vec::Vec;

// TODO: much of this can be made faster by avoiding the many small allocations in the
// bencode() functions, as well as removing the use of format!.
// These should likely not consume self;

pub trait Encode {
    fn bencode(self) -> Vec<u8>;
}

impl Encode for Data {
    fn bencode(mut self) -> Vec<u8> {
        let mut result = Vec::new();
        let size_as_str = self.len().to_string();
        result.extend_from_slice(size_as_str.as_str().as_bytes());
        result.push(':' as u8);
        result.append(&mut self);
        result
    }
}

impl Encode for String {
    fn bencode(self) -> Vec<u8> {
        self.as_str().bencode()
    }
}

impl Encode for &str {
    fn bencode(self) -> Vec<u8> {
        format!("{}:{}", self.len(), self).into_bytes()
    }
}

impl Encode for i64 {
    fn bencode(self) -> Vec<u8> {
        format!("i{}e", self).into_bytes()
    }
}

impl Encode for List {
    fn bencode(self) -> Vec<u8> {
        let mut result = Vec::new();
        result.push('l' as u8);
        for element in self {
            result.append(&mut element.bencode());
        }
        result.push('e' as u8);
        result
    }
}

impl Encode for Dictionary {
    fn bencode(self) -> Vec<u8> {
        let mut result = Vec::new();
        result.push('d' as u8);
        for (key, value) in self.iter() {
            //result.append(&mut key.bencode());
            result.append(&mut (*key).clone().bencode());
            result.append(&mut (*value).clone().bencode());
        }
        result.push('e' as u8);
        result
    }
}

impl Encode for DataKind {
    fn bencode(self) -> Vec<u8> {
        match self {
            DataKind::Data(value) => value.bencode(),
            DataKind::Integer(value) => value.bencode(),
            DataKind::List(value) => value.bencode(),
            DataKind::Dictionary(value) => value.bencode(),
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
        let result = (-3 as i64).bencode();
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
        //assert_eq!(expected, result.as_slice());
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
