use std::{collections::BTreeMap, convert::TryInto, ffi::CStr, ffi::CString, ptr};

use crate::{
    datatypes::{
        DataType, Extension, Field, IntegerType, IntervalUnit, Metadata, TimeUnit, UnionMode,
    },
    error::{ArrowError, Result},
};

use super::ArrowSchema;

#[allow(dead_code)]
struct SchemaPrivateData {
    name: CString,
    format: CString,
    metadata: Option<Vec<u8>>,
    children_ptr: Box<[*mut ArrowSchema]>,
    dictionary: Option<*mut ArrowSchema>,
}

// callback used to drop [ArrowSchema] when it is exported.
unsafe extern "C" fn c_release_schema(schema: *mut ArrowSchema) {
    if schema.is_null() {
        return;
    }
    let schema = &mut *schema;

    let private = Box::from_raw(schema.private_data as *mut SchemaPrivateData);
    for child in private.children_ptr.iter() {
        let _ = Box::from_raw(*child);
    }

    if let Some(ptr) = private.dictionary {
        let _ = Box::from_raw(ptr);
    }

    schema.release = None;
}

impl ArrowSchema {
    /// creates a new [ArrowSchema]
    pub(crate) fn new(field: &Field) -> Self {
        let format = to_format(field.data_type());
        let name = field.name.clone();

        let mut flags = field.is_nullable as i64 * 2;

        // allocate (and hold) the children
        let children_vec = match field.data_type() {
            DataType::List(field) => {
                vec![Box::new(ArrowSchema::new(field.as_ref()))]
            }
            DataType::FixedSizeList(field, _) => {
                vec![Box::new(ArrowSchema::new(field.as_ref()))]
            }
            DataType::LargeList(field) => {
                vec![Box::new(ArrowSchema::new(field.as_ref()))]
            }
            DataType::Map(field, is_sorted) => {
                flags += (*is_sorted as i64) * 4;
                vec![Box::new(ArrowSchema::new(field.as_ref()))]
            }
            DataType::Struct(fields) => fields
                .iter()
                .map(|field| Box::new(ArrowSchema::new(field)))
                .collect::<Vec<_>>(),
            DataType::Union(fields, _, _) => fields
                .iter()
                .map(|field| Box::new(ArrowSchema::new(field)))
                .collect::<Vec<_>>(),
            _ => vec![],
        };
        // note: this cannot be done along with the above because the above is fallible and this op leaks.
        let children_ptr = children_vec
            .into_iter()
            .map(Box::into_raw)
            .collect::<Box<_>>();
        let n_children = children_ptr.len() as i64;

        let dictionary = if let DataType::Dictionary(_, values, is_ordered) = field.data_type() {
            flags += *is_ordered as i64;
            // we do not store field info in the dict values, so can't recover it all :(
            let field = Field::new("", values.as_ref().clone(), true);
            Some(Box::new(ArrowSchema::new(&field)))
        } else {
            None
        };

        let metadata = &field.metadata;

        let metadata = if let DataType::Extension(name, _, extension_metadata) = field.data_type() {
            // append extension information.
            let mut metadata = metadata.clone();

            // metadata
            if let Some(extension_metadata) = extension_metadata {
                metadata.insert(
                    "ARROW:extension:metadata".to_string(),
                    extension_metadata.clone(),
                );
            }

            metadata.insert("ARROW:extension:name".to_string(), name.clone());

            Some(metadata_to_bytes(&metadata))
        } else if !metadata.is_empty() {
            Some(metadata_to_bytes(metadata))
        } else {
            None
        };

        let name = CString::new(name).unwrap();
        let format = CString::new(format).unwrap();

        let mut private = Box::new(SchemaPrivateData {
            name,
            format,
            metadata,
            children_ptr,
            dictionary: dictionary.map(Box::into_raw),
        });

        // <https://arrow.apache.org/docs/format/CDataInterface.html#c.ArrowSchema>
        Self {
            format: private.format.as_ptr(),
            name: private.name.as_ptr(),
            metadata: private
                .metadata
                .as_ref()
                .map(|x| x.as_ptr())
                .unwrap_or(std::ptr::null()) as *const ::std::os::raw::c_char,
            flags,
            n_children,
            children: private.children_ptr.as_mut_ptr(),
            dictionary: private.dictionary.unwrap_or(std::ptr::null_mut()),
            release: Some(c_release_schema),
            private_data: Box::into_raw(private) as *mut ::std::os::raw::c_void,
        }
    }

    /// create an empty [ArrowSchema]
    pub fn empty() -> Self {
        Self {
            format: std::ptr::null_mut(),
            name: std::ptr::null_mut(),
            metadata: std::ptr::null_mut(),
            flags: 0,
            n_children: 0,
            children: ptr::null_mut(),
            dictionary: std::ptr::null_mut(),
            release: None,
            private_data: std::ptr::null_mut(),
        }
    }

    /// returns the format of this schema.
    pub(crate) fn format(&self) -> &str {
        assert!(!self.format.is_null());
        // safe because the lifetime of `self.format` equals `self`
        unsafe { CStr::from_ptr(self.format) }
            .to_str()
            .expect("The external API has a non-utf8 as format")
    }

    /// returns the name of this schema.
    pub(crate) fn name(&self) -> &str {
        assert!(!self.name.is_null());
        // safe because the lifetime of `self.name` equals `self`
        unsafe { CStr::from_ptr(self.name) }.to_str().unwrap()
    }

    pub(crate) fn child(&self, index: usize) -> &'static Self {
        assert!(index < self.n_children as usize);
        assert!(!self.name.is_null());
        unsafe { self.children.add(index).as_ref().unwrap().as_ref().unwrap() }
    }

    pub(crate) fn dictionary(&self) -> Option<&'static Self> {
        if self.dictionary.is_null() {
            return None;
        };
        Some(unsafe { self.dictionary.as_ref().unwrap() })
    }

    pub(crate) fn nullable(&self) -> bool {
        (self.flags / 2) & 1 == 1
    }
}

impl Drop for ArrowSchema {
    fn drop(&mut self) {
        match self.release {
            None => (),
            Some(release) => unsafe { release(self) },
        };
    }
}

pub(crate) unsafe fn to_field(schema: &ArrowSchema) -> Result<Field> {
    let dictionary = schema.dictionary();
    let data_type = if let Some(dictionary) = dictionary {
        let indices = to_integer_type(schema.format())?;
        let values = to_field(dictionary)?;
        let is_ordered = schema.flags & 1 == 1;
        DataType::Dictionary(indices, Box::new(values.data_type().clone()), is_ordered)
    } else {
        to_data_type(schema)?
    };
    let (metadata, extension) = unsafe { metadata_from_bytes(schema.metadata) };

    let data_type = if let Some((name, extension_metadata)) = extension {
        DataType::Extension(name, Box::new(data_type), extension_metadata)
    } else {
        data_type
    };

    Ok(Field::new(schema.name(), data_type, schema.nullable()).with_metadata(metadata))
}

fn to_integer_type(format: &str) -> Result<IntegerType> {
    use IntegerType::*;
    Ok(match format {
        "c" => Int8,
        "C" => UInt8,
        "s" => Int16,
        "S" => UInt16,
        "i" => Int32,
        "I" => UInt32,
        "l" => Int64,
        "L" => UInt64,
        _ => {
            return Err(ArrowError::OutOfSpec(
                "Dictionary indices can only be integers".to_string(),
            ))
        }
    })
}

unsafe fn to_data_type(schema: &ArrowSchema) -> Result<DataType> {
    Ok(match schema.format() {
        "n" => DataType::Null,
        "b" => DataType::Boolean,
        "c" => DataType::Int8,
        "C" => DataType::UInt8,
        "s" => DataType::Int16,
        "S" => DataType::UInt16,
        "i" => DataType::Int32,
        "I" => DataType::UInt32,
        "l" => DataType::Int64,
        "L" => DataType::UInt64,
        "e" => DataType::Float16,
        "f" => DataType::Float32,
        "g" => DataType::Float64,
        "z" => DataType::Binary,
        "Z" => DataType::LargeBinary,
        "u" => DataType::Utf8,
        "U" => DataType::LargeUtf8,
        "tdD" => DataType::Date32,
        "tdm" => DataType::Date64,
        "tts" => DataType::Time32(TimeUnit::Second),
        "ttm" => DataType::Time32(TimeUnit::Millisecond),
        "ttu" => DataType::Time64(TimeUnit::Microsecond),
        "ttn" => DataType::Time64(TimeUnit::Nanosecond),
        "tDs" => DataType::Duration(TimeUnit::Second),
        "tDm" => DataType::Duration(TimeUnit::Millisecond),
        "tDu" => DataType::Duration(TimeUnit::Microsecond),
        "tDn" => DataType::Duration(TimeUnit::Nanosecond),
        "tiM" => DataType::Interval(IntervalUnit::YearMonth),
        "tiD" => DataType::Interval(IntervalUnit::DayTime),
        "+l" => {
            let child = schema.child(0);
            DataType::List(Box::new(to_field(child)?))
        }
        "+L" => {
            let child = schema.child(0);
            DataType::LargeList(Box::new(to_field(child)?))
        }
        "+m" => {
            let child = schema.child(0);

            let is_sorted = (schema.flags & 4) != 0;
            DataType::Map(Box::new(to_field(child)?), is_sorted)
        }
        "+s" => {
            let children = (0..schema.n_children as usize)
                .map(|x| to_field(schema.child(x)))
                .collect::<Result<Vec<_>>>()?;
            DataType::Struct(children)
        }
        other => {
            let parts = other.split(':').collect::<Vec<_>>();
            if parts.len() == 2 && parts[0] == "tss" {
                DataType::Timestamp(TimeUnit::Second, Some(parts[1].to_string()))
            } else if parts.len() == 2 && parts[0] == "tsm" {
                DataType::Timestamp(TimeUnit::Millisecond, Some(parts[1].to_string()))
            } else if parts.len() == 2 && parts[0] == "tsu" {
                DataType::Timestamp(TimeUnit::Microsecond, Some(parts[1].to_string()))
            } else if parts.len() == 2 && parts[0] == "tsn" {
                DataType::Timestamp(TimeUnit::Nanosecond, Some(parts[1].to_string()))
            } else if parts.len() == 2 && parts[0] == "w" {
                let size = parts[1].parse::<usize>().map_err(|_| {
                    ArrowError::OutOfSpec("size is not a valid integer".to_string())
                })?;
                DataType::FixedSizeBinary(size)
            } else if parts.len() == 2 && parts[0] == "+w" {
                let size = parts[1].parse::<usize>().map_err(|_| {
                    ArrowError::OutOfSpec("size is not a valid integer".to_string())
                })?;
                let child = to_field(schema.child(0))?;
                DataType::FixedSizeList(Box::new(child), size)
            } else if parts.len() == 2 && parts[0] == "d" {
                let parts = parts[1].split(',').collect::<Vec<_>>();
                if parts.len() < 2 || parts.len() > 3 {
                    return Err(ArrowError::OutOfSpec(
                        "Decimal must contain 2 or 3 comma-separated values".to_string(),
                    ));
                };
                if parts.len() == 3 {
                    let bit_width = parts[0].parse::<usize>().map_err(|_| {
                        ArrowError::OutOfSpec(
                            "Decimal bit width is not a valid integer".to_string(),
                        )
                    })?;
                    if bit_width != 128 {
                        return Err(ArrowError::OutOfSpec(
                            "Decimal256 is not supported".to_string(),
                        ));
                    }
                }
                let precision = parts[0].parse::<usize>().map_err(|_| {
                    ArrowError::OutOfSpec("Decimal precision is not a valid integer".to_string())
                })?;
                let scale = parts[1].parse::<usize>().map_err(|_| {
                    ArrowError::OutOfSpec("Decimal scale is not a valid integer".to_string())
                })?;
                DataType::Decimal(precision, scale)
            } else if !parts.is_empty() && ((parts[0] == "+us") || (parts[0] == "+ud")) {
                // union
                let mode = UnionMode::sparse(parts[0] == "+us");
                let type_ids = parts[1]
                    .split(',')
                    .map(|x| {
                        x.parse::<i32>().map_err(|_| {
                            ArrowError::OutOfSpec(
                                "Union type id is not a valid integer".to_string(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                let fields = (0..schema.n_children as usize)
                    .map(|x| to_field(schema.child(x)))
                    .collect::<Result<Vec<_>>>()?;
                DataType::Union(fields, Some(type_ids), mode)
            } else {
                return Err(ArrowError::OutOfSpec(format!(
                    "The datatype \"{}\" is still not supported in Rust implementation",
                    other
                )));
            }
        }
    })
}

/// the inverse of [to_field]
fn to_format(data_type: &DataType) -> String {
    match data_type {
        DataType::Null => "n".to_string(),
        DataType::Boolean => "b".to_string(),
        DataType::Int8 => "c".to_string(),
        DataType::UInt8 => "C".to_string(),
        DataType::Int16 => "s".to_string(),
        DataType::UInt16 => "S".to_string(),
        DataType::Int32 => "i".to_string(),
        DataType::UInt32 => "I".to_string(),
        DataType::Int64 => "l".to_string(),
        DataType::UInt64 => "L".to_string(),
        DataType::Float16 => "e".to_string(),
        DataType::Float32 => "f".to_string(),
        DataType::Float64 => "g".to_string(),
        DataType::Binary => "z".to_string(),
        DataType::LargeBinary => "Z".to_string(),
        DataType::Utf8 => "u".to_string(),
        DataType::LargeUtf8 => "U".to_string(),
        DataType::Date32 => "tdD".to_string(),
        DataType::Date64 => "tdm".to_string(),
        DataType::Time32(TimeUnit::Second) => "tts".to_string(),
        DataType::Time32(TimeUnit::Millisecond) => "ttm".to_string(),
        DataType::Time32(_) => {
            unreachable!("Time32 is only supported for seconds and milliseconds")
        }
        DataType::Time64(TimeUnit::Microsecond) => "ttu".to_string(),
        DataType::Time64(TimeUnit::Nanosecond) => "ttn".to_string(),
        DataType::Time64(_) => {
            unreachable!("Time64 is only supported for micro and nanoseconds")
        }
        DataType::Duration(TimeUnit::Second) => "tDs".to_string(),
        DataType::Duration(TimeUnit::Millisecond) => "tDm".to_string(),
        DataType::Duration(TimeUnit::Microsecond) => "tDu".to_string(),
        DataType::Duration(TimeUnit::Nanosecond) => "tDn".to_string(),
        DataType::Interval(IntervalUnit::YearMonth) => "tiM".to_string(),
        DataType::Interval(IntervalUnit::DayTime) => "tiD".to_string(),
        DataType::Interval(IntervalUnit::MonthDayNano) => {
            todo!("Spec for FFI for MonthDayNano still not defined.")
        }
        DataType::Timestamp(unit, tz) => {
            let unit = match unit {
                TimeUnit::Second => "s",
                TimeUnit::Millisecond => "m",
                TimeUnit::Microsecond => "u",
                TimeUnit::Nanosecond => "n",
            };
            format!(
                "ts{}:{}",
                unit,
                tz.as_ref().map(|x| x.as_ref()).unwrap_or("")
            )
        }
        DataType::Decimal(precision, scale) => format!("d:{},{}", precision, scale),
        DataType::List(_) => "+l".to_string(),
        DataType::LargeList(_) => "+L".to_string(),
        DataType::Struct(_) => "+s".to_string(),
        DataType::FixedSizeBinary(size) => format!("w:{}", size),
        DataType::FixedSizeList(_, size) => format!("+w:{}", size),
        DataType::Union(f, ids, mode) => {
            let sparsness = if mode.is_sparse() { 's' } else { 'd' };
            let mut r = format!("+u{}:", sparsness);
            let ids = if let Some(ids) = ids {
                ids.iter()
                    .fold(String::new(), |a, b| a + &b.to_string() + ",")
            } else {
                (0..f.len()).fold(String::new(), |a, b| a + &b.to_string() + ",")
            };
            let ids = &ids[..ids.len() - 1]; // take away last ","
            r.push_str(ids);
            r
        }
        DataType::Map(_, _) => "+m".to_string(),
        DataType::Dictionary(index, _, _) => to_format(&(*index).into()),
        DataType::Extension(_, inner, _) => to_format(inner.as_ref()),
    }
}

pub(super) fn get_child(data_type: &DataType, index: usize) -> Result<DataType> {
    match (index, data_type) {
        (0, DataType::List(field)) => Ok(field.data_type().clone()),
        (0, DataType::FixedSizeList(field, _)) => Ok(field.data_type().clone()),
        (0, DataType::LargeList(field)) => Ok(field.data_type().clone()),
        (0, DataType::Map(field, _)) => Ok(field.data_type().clone()),
        (index, DataType::Struct(fields)) => Ok(fields[index].data_type().clone()),
        (index, DataType::Union(fields, _, _)) => Ok(fields[index].data_type().clone()),
        (child, data_type) => Err(ArrowError::OutOfSpec(format!(
            "Requested child {} to type {:?} that has no such child",
            child, data_type
        ))),
    }
}

fn metadata_to_bytes(metadata: &BTreeMap<String, String>) -> Vec<u8> {
    let a = (metadata.len() as i32).to_ne_bytes().to_vec();
    metadata.iter().fold(a, |mut acc, (key, value)| {
        acc.extend((key.len() as i32).to_ne_bytes());
        acc.extend(key.as_bytes());
        acc.extend((value.len() as i32).to_ne_bytes());
        acc.extend(value.as_bytes());
        acc
    })
}

unsafe fn read_ne_i32(ptr: *const u8) -> i32 {
    let slice = std::slice::from_raw_parts(ptr, 4);
    i32::from_ne_bytes(slice.try_into().unwrap())
}

unsafe fn read_bytes(ptr: *const u8, len: usize) -> &'static str {
    let slice = std::slice::from_raw_parts(ptr, len);
    simdutf8::basic::from_utf8(slice).unwrap()
}

unsafe fn metadata_from_bytes(data: *const ::std::os::raw::c_char) -> (Metadata, Extension) {
    let mut data = data as *const u8; // u8 = i8
    if data.is_null() {
        return (Metadata::default(), None);
    };
    let len = read_ne_i32(data);
    data = data.add(4);

    let mut result = BTreeMap::new();
    let mut extension_name = None;
    let mut extension_metadata = None;
    for _ in 0..len {
        let key_len = read_ne_i32(data) as usize;
        data = data.add(4);
        let key = read_bytes(data, key_len);
        data = data.add(key_len);
        let value_len = read_ne_i32(data) as usize;
        data = data.add(4);
        let value = read_bytes(data, value_len);
        data = data.add(value_len);
        match key {
            "ARROW:extension:name" => {
                extension_name = Some(value.to_string());
            }
            "ARROW:extension:metadata" => {
                extension_metadata = Some(value.to_string());
            }
            _ => {
                result.insert(key.to_string(), value.to_string());
            }
        };
    }
    let extension = extension_name.map(|name| (name, extension_metadata));
    (result, extension)
}
