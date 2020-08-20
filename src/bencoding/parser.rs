use std::io::prelude::*;
use std::path::Path;

use super::data_kind;

use data_kind::DataKind;
use data_kind::Dictionary;

pub struct Parser<'a> {
    current_index: usize,
    data: &'a [u8],
    //error: String,
}

impl<'a> Parser<'a> {
    fn create_error(&self, context: &[u8], offset: usize, kind: ParseErrorKind) -> ParseError {
        let error = match kind {
            ParseErrorKind::ParseIntError(ref error) => error.to_string(),
            ParseErrorKind::Utf8Error(error) => error.to_string(),
            ParseErrorKind::MissingTerminator => "missing terminator".into(),
            ParseErrorKind::MissingLengthPrefix => "missing length prefix".into(),
        };
        let range = 5;
        let index = self.current_index + offset;
        let right = if context.len() > index + range {
            context.len() - 1
        } else {
            index + range
        };
        let message = format!(
            "{} at index {} of input; context: ...{}...",
            error,
            index,
            String::from_utf8_lossy(&self.data[index..right])
        );
        ParseError { kind, message }
    }

    fn get_length_prefix(&self, data: &[u8]) -> Result<(usize, usize), ParseError> {
        if let Some(index) = data.iter().position(|c| *c == ':' as u8) {
            let as_str = self.to_string(&data[0..index])?;
            Ok((self.parse_usize(as_str)?, index + 1))
        } else {
            self.create_error(data, 0, ParseErrorKind::MissingLengthPrefix)
                .into()
        }
    }

    pub fn new(data: &[u8]) -> Parser {
        return Parser {
            current_index: 0,
            data: data,
            //error: "".into(),
        };
    }

    fn parse_dictionary(&mut self, data: &[u8]) -> Result<(Dictionary, usize), ParseError> {
        debug_assert!(data[0] == 'd' as u8);
        let mut result = Dictionary::new();
        let mut index = 1;
        while data[index] != 'e' as u8 {
            let (key, spent) = self.parse_string(&data[index..])?;
            index += spent;
            let (value, spent) = self.parse(&data[index..])?;
            index += spent;
            result.insert(key, value);
        }
        Ok((result, index + 1))
    }

    fn parse_integer(&self, data: &[u8]) -> Result<(i64, usize), ParseError> {
        debug_assert!(data[0] == 'i' as u8);
        if let Some(index) = data.iter().position(|c| *c == 'e' as u8) {
            let as_str = self.to_string(&data[1..index])?;
            let parsed = self.parse_i64(as_str)?;
            Ok((parsed, index + 1))
        } else {
            self.create_error(data, 0, ParseErrorKind::MissingTerminator)
                .into()
        }
    }

    fn parse_list(&mut self, data: &[u8]) -> Result<(Vec<DataKind>, usize), ParseError> {
        debug_assert!(data[0] == 'l' as u8);
        let mut index = 1;
        let mut result = Vec::new();
        while data[index] != 'e' as u8 {
            let (data, spent) = self.parse(&data[index..])?;
            index += spent;
            result.push(data);
        }
        Ok((result, index + 1)) // add one for final 'e'
    }

    fn parse_string(&self, data: &[u8]) -> Result<(String, usize), ParseError> {
        let (length, spent) = self.get_length_prefix(data)?;
        let start = spent;
        let end = start + length;
        if start >= data.len() || end > data.len() {
            self.create_error(data, 0, ParseErrorKind::MissingTerminator)
                .into()
        } else {
            let text = self.to_string(&data[start..end])?;
            Ok((text.into(), end))
        }
    }

    fn parse_data(&self, data: &[u8]) -> Result<(Vec<u8>, usize), ParseError> {
        let (length, spent) = self.get_length_prefix(data)?;
        let start = spent;
        let end = start + length;
        if start >= data.len() || end > data.len() {
            self.create_error(data, 0, ParseErrorKind::MissingTerminator)
                .into()
        } else {
            Ok((data[start..end].to_vec(), end))
        }
    }

    // TODO: this is almost identical to parse_usize
    fn parse_i64(&self, text: &str) -> Result<i64, ParseError> {
        match text.parse::<i64>() {
            Ok(value) => Ok(value),
            Err(error) => self
                .create_error(text.as_bytes(), 0, ParseErrorKind::ParseIntError(error))
                .into(),
        }
    }

    fn parse_usize(&self, text: &str) -> Result<usize, ParseError> {
        match text.parse::<usize>() {
            Ok(value) => Ok(value),
            Err(error) => self
                .create_error(text.as_bytes(), 0, ParseErrorKind::ParseIntError(error))
                .into(),
        }
    }

    pub fn parse(&mut self, data: &[u8]) -> Result<(DataKind, usize), ParseError> {
        let c = data[0] as char;
        macro_rules! MatchCase {
            ($self:ident, $F:ident) => {{
                let (result, spent) = $self.$F(data)?;
                (DataKind::from(result), spent)
            }};
        }
        let (result, spent) = match c {
            'd' => MatchCase!(self, parse_dictionary),
            'i' => MatchCase!(self, parse_integer),
            'l' => MatchCase!(self, parse_list),
            _ => MatchCase!(self, parse_data),
        };
        self.current_index += spent;
        Ok((result, spent))
    }

    fn to_string<'b>(&self, data: &'b [u8]) -> Result<&'b str, ParseError> {
        match std::str::from_utf8(data) {
            Ok(value) => Ok(value),
            Err(value) => self.create_error(data, 0, value.into()).into(),
        }
    }
}

pub fn parse(data: &[u8]) -> Result<DataKind, ParseError> {
    let mut parser = Parser::new(data);
    let (result, _) = parser.parse(data)?;
    Ok(result)
}

pub fn parse_into_dictionary(
    torrent_file: &Path,
) -> Result<Dictionary, Box<dyn std::error::Error>> {
    let mut file = std::fs::File::open(torrent_file)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;
    let result = parse(&buffer).unwrap();
    if let DataKind::Dictionary(value) = result {
        Ok(value)
    } else {
        Err(String::from("File needs to be a bencoded dictionary").into())
    }
}

#[derive(Debug)]
enum ParseErrorKind {
    ParseIntError(std::num::ParseIntError),
    Utf8Error(std::str::Utf8Error),
    MissingTerminator,
    MissingLengthPrefix,
}

#[derive(Debug)]
pub struct ParseError {
    kind: ParseErrorKind,
    message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

impl From<std::num::ParseIntError> for ParseErrorKind {
    fn from(error: std::num::ParseIntError) -> Self {
        ParseErrorKind::ParseIntError(error)
    }
}

impl From<std::str::Utf8Error> for ParseErrorKind {
    fn from(error: std::str::Utf8Error) -> Self {
        ParseErrorKind::Utf8Error(error)
    }
}

impl<T> From<ParseError> for Result<T, ParseError> {
    fn from(error: ParseError) -> Self {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use super::DataKind;
    use super::Dictionary;

    #[test]
    fn parse_integer_test() {
        let text = "i-3e".as_bytes();
        let result = parse(&text).unwrap();
        assert_eq!(result.as_int().unwrap(), -3);
    }
    #[test]
    fn parse_string_test() {
        let text = "3:cow".as_bytes();
        let result = parse(&text).unwrap();
        assert_eq!(result.as_utf8().unwrap(), "cow");
    }
    #[test]
    fn parse_list_test() {
        let text = "l4:spam4:eggse".as_bytes();
        let result = parse(&text).unwrap();
        let expected = vec![DataKind::Data("spam".into()), DataKind::Data("eggs".into())];
        assert_eq!(*result.as_list().unwrap(), expected);
    }
    #[test]
    fn parse_list_of_list_test() {
        let text = "ll4:spam4:eggse4:spam4:eggse".as_bytes();
        let result = parse(&text).unwrap();
        let expected = vec![
            DataKind::List(vec![
                DataKind::Data("spam".into()),
                DataKind::Data("eggs".into()),
            ]),
            DataKind::Data("spam".into()),
            DataKind::Data("eggs".into()),
        ];
        assert_eq!(*result.as_list().unwrap(), expected);
    }
    #[test]
    fn parse_dictionary_test() {
        let text = "d3:cow3:moo4:spam4:eggse".as_bytes();
        let result = parse(&text).unwrap();
        let mut map = Dictionary::new();
        map.insert("cow".into(), DataKind::Data("moo".into()));
        map.insert("spam".into(), DataKind::Data("eggs".into()));
        let expected = DataKind::Dictionary(map);
        assert_eq!(result, expected);
    }
    #[test]
    fn parse_dictionary_with_dictionary_test() {
        let text = "d3:cow3:moo4:spamd3:cow3:mooee".as_bytes();
        let result = parse(&text).unwrap();
        let mut map = Dictionary::new();
        map.insert("cow".into(), DataKind::Data("moo".into()));
        map.insert("spam".into(), DataKind::Dictionary(map.clone()));
        let expected = DataKind::Dictionary(map);
        assert_eq!(result, expected);
    }
}
