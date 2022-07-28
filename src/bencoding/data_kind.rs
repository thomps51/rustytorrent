use std::collections::btree_map::Iter;
use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::convert::TryInto;
use std::error::Error;
use std::ops::Index;

#[derive(Debug, PartialEq, Clone)]
pub enum DataKind {
    Data(Data),
    Integer(i64),
    List(List),
    Dictionary(Dictionary),
}

pub type Data = Vec<u8>;
// pub type Dictionary = BTreeMap<String, DataKind>;
pub type List = Vec<DataKind>;

#[derive(Debug, PartialEq, Clone)]
pub struct Dictionary {
    data: BTreeMap<String, DataKind>,
}

impl Index<&str> for Dictionary {
    type Output = DataKind;

    fn index(&self, key: &str) -> &DataKind {
        self.get(key).expect("no entry found for key")
    }
}

impl Dictionary {
    pub fn new() -> Self {
        Dictionary {
            data: BTreeMap::new(),
        }
    }

    pub fn insert<T: Into<DataKind>>(&mut self, key: String, value: T) {
        self.data.insert(key, value.into());
    }

    pub fn get(&self, key: &str) -> Option<&DataKind> {
        self.data.get(key)
    }

    pub fn get_as<T>(&self, name: &str) -> Result<Option<T>, Box<dyn Error>>
    where
        T: TryFrom<DataKind, Error = ConvertError>,
    {
        if let Some(v) = self.get(name) {
            Ok(Some(v.clone().try_into()?))
        } else {
            Ok(None)
        }
    }

    pub fn iter(&self) -> Iter<String, DataKind> {
        self.data.iter()
    }
}

impl DataKind {
    pub fn as_utf8(&self) -> Result<&str, Box<dyn Error>> {
        if let Self::Data(value) = self {
            Ok(std::str::from_utf8(value)?)
        } else {
            Err(String::from("DataKind is not Data").into())
        }
    }
    pub fn as_bytes(&self) -> Result<&[u8], Box<dyn Error>> {
        if let Self::Data(value) = self {
            Ok(value)
        } else {
            Err(String::from("DataKind is not Data").into())
        }
    }

    pub fn as_int(&self) -> Result<i64, Box<dyn Error>> {
        if let Self::Integer(value) = self {
            Ok(*value)
        } else {
            Err(String::from("DataKind is not Integer").into())
        }
    }

    pub fn as_list(&self) -> Result<&List, Box<dyn Error>> {
        if let Self::List(value) = self {
            Ok(value)
        } else {
            Err(String::from("DataKind is not List").into())
        }
    }

    pub fn as_dict(&self) -> Result<&Dictionary, Box<dyn Error>> {
        if let Self::Dictionary(value) = self {
            Ok(value)
        } else {
            Err(String::from("DataKind is not Dictionary").into())
        }
    }
}

pub fn get_as<T>(dict: &Dictionary, name: &str) -> Result<T, Box<dyn Error>>
where
    T: TryFrom<DataKind, Error = ConvertError>,
{
    if let Some(v) = dict.get(name) {
        Ok(v.clone().try_into()?)
    } else {
        Err(format!("dictionary is missing '{}' key", name).into())
    }
}

macro_rules! ImplTryFrom {
    ($A:ident, $B:ident) => {
        impl TryFrom<DataKind> for $A {
            type Error = ConvertError;
            fn try_from(value: DataKind) -> Result<$A, Self::Error> {
                if let DataKind::$B(v) = value {
                    Ok(v)
                } else {
                    Err(ConvertError::new(concat!(
                        "DataKind is not",
                        stringify!($B)
                    )))
                }
            }
        }
    };
}

#[derive(Debug)]
pub struct ConvertError {
    message: &'static str,
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConvertError {}

impl ConvertError {
    fn new(message: &'static str) -> ConvertError {
        ConvertError { message }
    }
}

ImplTryFrom!(i64, Integer);
ImplTryFrom!(List, List);
ImplTryFrom!(Dictionary, Dictionary);
ImplTryFrom!(Data, Data);

impl TryFrom<DataKind> for String {
    type Error = ConvertError;

    fn try_from(value: DataKind) -> Result<String, Self::Error> {
        if let DataKind::Data(v) = value {
            if let Ok(text) = String::from_utf8(v) {
                Ok(text)
            } else {
                Err(ConvertError::new("DataKind is Data, but not valid UTF-8"))
            }
        } else {
            Err(ConvertError::new("DataKind is not Data"))
        }
    }
}

impl TryFrom<DataKind> for usize {
    type Error = ConvertError;

    fn try_from(value: DataKind) -> Result<usize, Self::Error> {
        if let DataKind::Integer(v) = value {
            if let Ok(us) = v.try_into() {
                return Ok(us);
            } else {
                Err(ConvertError::new("DataKind is Integer, but negative"))
            }
        } else {
            Err(ConvertError::new("DataKind is not Integer"))
        }
    }
}

macro_rules! ImplFrom {
    ($A:ident, $B:ident) => {
        impl From<$A> for DataKind {
            fn from(value: $A) -> Self {
                DataKind::$B(value)
            }
        }
    };
}

// Direct conversions
ImplFrom!(i64, Integer);
ImplFrom!(Dictionary, Dictionary);
ImplFrom!(List, List);
ImplFrom!(Data, Data);

// Indirect conversions
impl From<String> for DataKind {
    fn from(value: String) -> Self {
        DataKind::Data(value.into_bytes())
    }
}

// Indirect conversions
impl From<usize> for DataKind {
    fn from(value: usize) -> Self {
        DataKind::Integer(value as i64)
    }
}
