//! contains the [`Scalar`] trait object representing individual items of [`Array`](crate::array::Array)s,
//! as well as concrete implementations such as [`BooleanScalar`].
use std::any::Any;

use crate::{array::*, datatypes::*};

mod dictionary;
pub use dictionary::*;
mod equal;
mod primitive;
pub use primitive::*;
mod utf8;
pub use utf8::*;
mod binary;
pub use binary::*;
mod boolean;
pub use boolean::*;
mod list;
pub use list::*;
mod null;
pub use null::*;
mod struct_;
pub use struct_::*;
mod fixed_size_list;
pub use fixed_size_list::*;
mod fixed_size_binary;
pub use fixed_size_binary::*;

/// Trait object declaring an optional value with a [`DataType`].
/// This strait is often used in APIs that accept multiple scalar types.
pub trait Scalar: std::fmt::Debug + Send + Sync {
    /// convert itself to
    fn as_any(&self) -> &dyn Any;

    /// whether it is valid
    fn is_valid(&self) -> bool;

    /// the logical type.
    fn data_type(&self) -> &DataType;
}

macro_rules! dyn_new_utf8 {
    ($array:expr, $index:expr, $type:ty) => {{
        let array = $array.as_any().downcast_ref::<Utf8Array<$type>>().unwrap();
        let value = if array.is_valid($index) {
            Some(array.value($index))
        } else {
            None
        };
        Box::new(Utf8Scalar::<$type>::new(value))
    }};
}

macro_rules! dyn_new_binary {
    ($array:expr, $index:expr, $type:ty) => {{
        let array = $array
            .as_any()
            .downcast_ref::<BinaryArray<$type>>()
            .unwrap();
        let value = if array.is_valid($index) {
            Some(array.value($index))
        } else {
            None
        };
        Box::new(BinaryScalar::<$type>::new(value))
    }};
}

macro_rules! dyn_new_list {
    ($array:expr, $index:expr, $type:ty) => {{
        let array = $array.as_any().downcast_ref::<ListArray<$type>>().unwrap();
        let value = if array.is_valid($index) {
            Some(array.value($index).into())
        } else {
            None
        };
        Box::new(ListScalar::<$type>::new(array.data_type().clone(), value))
    }};
}

/// creates a new [`Scalar`] from an [`Array`].
pub fn new_scalar(array: &dyn Array, index: usize) -> Box<dyn Scalar> {
    use PhysicalType::*;
    match array.data_type().to_physical_type() {
        Null => Box::new(NullScalar::new()),
        Boolean => {
            let array = array.as_any().downcast_ref::<BooleanArray>().unwrap();
            let value = if array.is_valid(index) {
                Some(array.value(index))
            } else {
                None
            };
            Box::new(BooleanScalar::new(value))
        }
        Primitive(primitive) => with_match_primitive_type!(primitive, |$T| {
            let array = array
                .as_any()
                .downcast_ref::<PrimitiveArray<$T>>()
                .unwrap();
            let value = if array.is_valid(index) {
                Some(array.value(index))
            } else {
                None
            };
            Box::new(PrimitiveScalar::new(array.data_type().clone(), value))
        }),
        Utf8 => dyn_new_utf8!(array, index, i32),
        LargeUtf8 => dyn_new_utf8!(array, index, i64),
        Binary => dyn_new_binary!(array, index, i32),
        LargeBinary => dyn_new_binary!(array, index, i64),
        List => dyn_new_list!(array, index, i32),
        LargeList => dyn_new_list!(array, index, i64),
        Struct => {
            let array = array.as_any().downcast_ref::<StructArray>().unwrap();
            if array.is_valid(index) {
                let values = array
                    .values()
                    .iter()
                    .map(|x| new_scalar(x.as_ref(), index).into())
                    .collect();
                Box::new(StructScalar::new(array.data_type().clone(), Some(values)))
            } else {
                Box::new(StructScalar::new(array.data_type().clone(), None))
            }
        }
        FixedSizeBinary => {
            let array = array
                .as_any()
                .downcast_ref::<FixedSizeBinaryArray>()
                .unwrap();
            let value = if array.is_valid(index) {
                Some(array.value(index))
            } else {
                None
            };
            Box::new(FixedSizeBinaryScalar::new(array.data_type().clone(), value))
        }
        FixedSizeList => {
            let array = array.as_any().downcast_ref::<FixedSizeListArray>().unwrap();
            let value = if array.is_valid(index) {
                Some(array.value(index).into())
            } else {
                None
            };
            Box::new(FixedSizeListScalar::new(array.data_type().clone(), value))
        }
        Union | Map => todo!(),
        Dictionary(key_type) => match_integer_type!(key_type, |$T| {
            let array = array
                .as_any()
                .downcast_ref::<DictionaryArray<$T>>()
                .unwrap();
            let value = if array.is_valid(index) {
                Some(array.value(index).into())
            } else {
                None
            };
            Box::new(DictionaryScalar::<$T>::new(
                array.data_type().clone(),
                value,
            ))
        }),
    }
}
