use crate::buffer::{Buffer, MutableBuffer};
use crate::{
    array::{Array, PrimitiveArray},
    bitmap::MutableBitmap,
    bits::SlicesIterator,
    types::NativeType,
};

use super::super::SortOptions;
use super::sort::{sort_by, sort_inner};

fn partial_sort_inner<T, F>(values: &mut [T], mut cmp: F, descending: bool, limit: usize)
where
    T: NativeType,
    F: FnMut(&T, &T) -> std::cmp::Ordering,
{
    if descending {
        let (before, _mid, _after) =
            values.select_nth_unstable_by(limit, |x, y| cmp(x, y).reverse());
        before.sort_unstable_by(|x, y| cmp(x, y).reverse());
    } else {
        let (before, _mid, _after) = values.select_nth_unstable_by(limit, &mut cmp);
        before.sort_unstable_by(cmp);
    }
}

/// Sorts a [`PrimitiveArray`] according to `cmp` comparator and [`SortOptions`] up to `limit`.
/// The sorted array ends up with `limit` slots.
pub fn partial_sort_by<T, F>(
    array: &PrimitiveArray<T>,
    cmp: F,
    options: &SortOptions,
    limit: usize,
) -> PrimitiveArray<T>
where
    T: NativeType,
    F: FnMut(&T, &T) -> std::cmp::Ordering,
{
    if limit >= array.len() {
        return sort_by(array, cmp, options);
    }

    let values = array.values();
    let validity = array.validity();

    let (buffer, validity) = if let Some(validity) = validity {
        let nulls = (0..validity.null_count()).map(|_| false);
        let valids = (validity.null_count()..array.len()).map(|_| true);
        let valids_len = array.len() - validity.null_count();

        let mut buffer = MutableBuffer::<T>::with_capacity(array.len());
        let mut new_validity = MutableBitmap::with_capacity(array.len());
        let slices = SlicesIterator::new(validity);

        if options.nulls_first {
            // validity
            nulls
                .chain(valids)
                .take(limit)
                .for_each(|value| unsafe { new_validity.push_unchecked(value) });

            // values
            if limit > validity.null_count() {
                (0..validity.null_count()).for_each(|_| buffer.push(T::default()));

                for (start, len) in slices {
                    buffer.extend_from_slice(&values[start..start + len])
                }

                partial_sort_inner(
                    &mut buffer.as_slice_mut()[validity.null_count()..],
                    cmp,
                    options.descending,
                    limit - validity.null_count(),
                );
                buffer.resize(limit, T::default());
            } else {
                (0..limit).for_each(|_| buffer.push(T::default()));
            }
        } else {
            // validity
            valids
                .chain(nulls)
                .take(limit)
                .for_each(|value| unsafe { new_validity.push_unchecked(value) });

            // values
            for (start, len) in slices {
                buffer.extend_from_slice(&values[start..start + len])
            }

            if limit > valids_len {
                sort_inner(buffer.as_slice_mut(), cmp, options.descending);
                (0..limit - valids_len).for_each(|_| buffer.push(T::default()));
            } else {
                partial_sort_inner(buffer.as_slice_mut(), cmp, options.descending, limit);
                debug_assert!(buffer.len() >= limit);
                buffer.resize(limit, T::default())
            }
        };

        (buffer.into(), new_validity.into())
    } else {
        let mut buffer = MutableBuffer::<T>::from(values);

        partial_sort_inner(&mut buffer.as_slice_mut(), cmp, options.descending, limit);

        let values: Buffer<T> = buffer.into();

        (values.slice(0, limit), None)
    };
    PrimitiveArray::<T>::from_data(array.data_type().clone(), buffer, validity)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::array::ord;
    use crate::array::Primitive;
    use crate::datatypes::DataType;

    fn test_partial_sort_primitive_arrays<T>(
        data: &[Option<T>],
        data_type: DataType,
        options: SortOptions,
        limit: usize,
        expected_data: &[Option<T>],
    ) where
        T: NativeType + std::cmp::Ord,
    {
        let input = Primitive::<T>::from(data).to(data_type.clone());
        let expected = Primitive::<T>::from(expected_data).to(data_type);
        let output = partial_sort_by(&input, ord::total_cmp, &options, limit);
        assert_eq!(expected, output)
    }

    #[test]
    fn ascending_nulls_last_limit() {
        test_partial_sort_primitive_arrays::<i8>(
            &[Some(2), None, None, Some(1)],
            DataType::Int8,
            SortOptions {
                descending: false,
                nulls_first: false,
            },
            3,
            &[Some(1), Some(2), None],
        );
    }

    #[test]
    fn descending_nulls_last_limit() {
        test_partial_sort_primitive_arrays::<i8>(
            &[Some(2), None, None, Some(1)],
            DataType::Int8,
            SortOptions {
                descending: true,
                nulls_first: false,
            },
            3,
            &[Some(2), Some(1), None],
        );
    }

    #[test]
    fn ascending_nulls_first_limit() {
        test_partial_sort_primitive_arrays::<i8>(
            &[Some(2), None, None, Some(1)],
            DataType::Int8,
            SortOptions {
                descending: false,
                nulls_first: true,
            },
            3,
            &[None, None, Some(1)],
        );
    }

    #[test]
    fn descending_nulls_first_limit() {
        test_partial_sort_primitive_arrays::<i8>(
            &[Some(2), None, None, Some(1)],
            DataType::Int8,
            SortOptions {
                descending: true,
                nulls_first: true,
            },
            3,
            &[None, None, Some(2)],
        );
    }
}
