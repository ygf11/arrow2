//! This library demonstrates a minimal usage of Rust's C data interface to pass
//! arrays from and to Python.
mod c_stream;

use std::error;
use std::fmt;
use std::sync::Arc;

use pyo3::exceptions::PyOSError;
use pyo3::ffi::Py_uintptr_t;
use pyo3::prelude::*;
use pyo3::wrap_pyfunction;

use arrow2::{array::Array, datatypes::Field, error::ArrowError, ffi};

/// an error that bridges ArrowError with a Python error
#[derive(Debug)]
enum PyO3ArrowError {
    ArrowError(ArrowError),
}

impl fmt::Display for PyO3ArrowError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            PyO3ArrowError::ArrowError(ref e) => e.fmt(f),
        }
    }
}

impl error::Error for PyO3ArrowError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            // The cause is the underlying implementation error type. Is implicitly
            // cast to the trait object `&error::Error`. This works because the
            // underlying type already implements the `Error` trait.
            PyO3ArrowError::ArrowError(ref e) => Some(e),
        }
    }
}

impl From<ArrowError> for PyO3ArrowError {
    fn from(err: ArrowError) -> PyO3ArrowError {
        PyO3ArrowError::ArrowError(err)
    }
}

impl From<PyO3ArrowError> for PyErr {
    fn from(err: PyO3ArrowError) -> PyErr {
        PyOSError::new_err(err.to_string())
    }
}

fn to_rust_array(ob: PyObject, py: Python) -> PyResult<Arc<dyn Array>> {
    // prepare a pointer to receive the Array struct
    let array = Box::new(ffi::ArrowArray::empty());
    let schema = Box::new(ffi::ArrowSchema::empty());

    let array_ptr = &*array as *const ffi::ArrowArray;
    let schema_ptr = &*schema as *const ffi::ArrowSchema;

    // make the conversion through PyArrow's private API
    // this changes the pointer's memory and is thus unsafe. In particular, `_export_to_c` can go out of bounds
    ob.call_method1(
        py,
        "_export_to_c",
        (array_ptr as Py_uintptr_t, schema_ptr as Py_uintptr_t),
    )?;

    let field = unsafe { ffi::import_field_from_c(schema.as_ref()).map_err(PyO3ArrowError::from)? };
    let array =
        unsafe { ffi::import_array_from_c(array, field.data_type).map_err(PyO3ArrowError::from)? };

    Ok(array.into())
}

fn to_py_array(array: Arc<dyn Array>, py: Python) -> PyResult<PyObject> {
    let array_ptr = Box::new(ffi::ArrowArray::empty());
    let schema_ptr = Box::new(ffi::ArrowSchema::empty());

    let array_ptr = Box::into_raw(array_ptr);
    let schema_ptr = Box::into_raw(schema_ptr);

    unsafe {
        ffi::export_field_to_c(&Field::new("", array.data_type().clone(), true), schema_ptr);
        ffi::export_array_to_c(array, array_ptr);
    };

    let pa = py.import("pyarrow")?;

    let array = pa.getattr("Array")?.call_method1(
        "_import_from_c",
        (array_ptr as Py_uintptr_t, schema_ptr as Py_uintptr_t),
    )?;

    unsafe {
        Box::from_raw(array_ptr);
        Box::from_raw(schema_ptr);
    };

    Ok(array.to_object(py))
}

fn to_rust_field(ob: PyObject, py: Python) -> PyResult<Field> {
    // prepare a pointer to receive the Array struct
    let schema = Box::new(ffi::ArrowSchema::empty());

    let schema_ptr = &*schema as *const ffi::ArrowSchema;

    // make the conversion through PyArrow's private API
    // this changes the pointer's memory and is thus unsafe. In particular, `_export_to_c` can go out of bounds
    ob.call_method1(py, "_export_to_c", (schema_ptr as Py_uintptr_t,))?;

    let field = unsafe { ffi::import_field_from_c(schema.as_ref()).map_err(PyO3ArrowError::from)? };

    Ok(field)
}

fn to_py_field(field: &Field, py: Python) -> PyResult<PyObject> {
    let schema_ptr = Box::new(ffi::ArrowSchema::empty());
    let schema_ptr = Box::into_raw(schema_ptr);

    unsafe {
        ffi::export_field_to_c(field, schema_ptr);
    };

    let pa = py.import("pyarrow")?;

    let array = pa
        .getattr("Field")?
        .call_method1("_import_from_c", (schema_ptr as Py_uintptr_t,))?;

    unsafe { Box::from_raw(schema_ptr) };

    Ok(array.to_object(py))
}

/// Converts to rust and back to python
#[pyfunction]
fn round_trip_array(array: PyObject, py: Python) -> PyResult<PyObject> {
    // import
    let array = to_rust_array(array, py)?;

    // export
    to_py_array(array, py)
}

/// Converts to rust and back to python
#[pyfunction]
fn round_trip_field(array: PyObject, py: Python) -> PyResult<PyObject> {
    // import
    let field = to_rust_field(array, py)?;

    // export
    to_py_field(&field, py)
}

#[pyfunction]
pub fn to_rust_iterator(ob: PyObject, py: Python) -> PyResult<Vec<PyObject>> {
    c_stream::to_rust_iterator(ob, py)
}

#[pyfunction]
pub fn from_rust_iterator(py: Python) -> PyResult<PyObject> {
    c_stream::from_rust_iterator(py)
}

#[pymodule]
fn arrow_pyarrow_integration_testing(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(round_trip_array, m)?)?;
    m.add_function(wrap_pyfunction!(round_trip_field, m)?)?;
    m.add_function(wrap_pyfunction!(to_rust_iterator, m)?)?;
    m.add_function(wrap_pyfunction!(from_rust_iterator, m)?)?;
    Ok(())
}
