use std::collections::VecDeque;

use parquet2::{
    deserialize::SliceFilteredIter, encoding::Encoding, page::DataPage, schema::Repetition,
};

use crate::{
    array::BooleanArray,
    bitmap::{utils::BitmapIter, MutableBitmap},
    datatypes::DataType,
    error::Result,
};

use super::super::utils;
use super::super::utils::{
    extend_from_decoder, get_selected_rows, next, split_buffer, DecodedState, Decoder,
    FilteredOptionalPageValidity, MaybeNext, OptionalPageValidity,
};
use super::super::DataPages;

#[derive(Debug)]
struct Values<'a>(BitmapIter<'a>);

impl<'a> Values<'a> {
    pub fn new(page: &'a DataPage) -> Self {
        let (_, _, values) = split_buffer(page);

        Self(BitmapIter::new(values, 0, values.len() * 8))
    }
}

// The state of a required DataPage with a boolean physical type
#[derive(Debug)]
struct Required<'a> {
    values: &'a [u8],
    // invariant: offset <= length;
    offset: usize,
    length: usize,
}

impl<'a> Required<'a> {
    pub fn new(page: &'a DataPage) -> Self {
        Self {
            values: page.buffer(),
            offset: 0,
            length: page.num_values(),
        }
    }
}

#[derive(Debug)]
struct FilteredRequired<'a> {
    values: SliceFilteredIter<BitmapIter<'a>>,
}

impl<'a> FilteredRequired<'a> {
    pub fn new(page: &'a DataPage) -> Self {
        // todo: replace this by an iterator over slices, for faster deserialization
        let values = BitmapIter::new(page.buffer(), 0, page.num_values());

        let rows = get_selected_rows(page);
        let values = SliceFilteredIter::new(values, rows);

        Self { values }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.values.size_hint().0
    }
}

// The state of a `DataPage` of `Boolean` parquet boolean type
#[derive(Debug)]
enum State<'a> {
    Optional(OptionalPageValidity<'a>, Values<'a>),
    Required(Required<'a>),
    FilteredRequired(FilteredRequired<'a>),
    FilteredOptional(FilteredOptionalPageValidity<'a>, Values<'a>),
}

impl<'a> State<'a> {
    pub fn len(&self) -> usize {
        match self {
            State::Optional(validity, _) => validity.len(),
            State::Required(page) => page.length - page.offset,
            State::FilteredRequired(page) => page.len(),
            State::FilteredOptional(optional, _) => optional.len(),
        }
    }
}

impl<'a> utils::PageState<'a> for State<'a> {
    fn len(&self) -> usize {
        self.len()
    }
}

impl<'a> DecodedState<'a> for (MutableBitmap, MutableBitmap) {
    fn len(&self) -> usize {
        self.0.len()
    }
}

#[derive(Default)]
struct BooleanDecoder {}

impl<'a> Decoder<'a> for BooleanDecoder {
    type State = State<'a>;
    type DecodedState = (MutableBitmap, MutableBitmap);

    fn build_state(&self, page: &'a DataPage) -> Result<Self::State> {
        let is_optional =
            page.descriptor.primitive_type.field_info.repetition == Repetition::Optional;
        let is_filtered = page.selected_rows().is_some();

        match (page.encoding(), is_optional, is_filtered) {
            (Encoding::Plain, true, false) => Ok(State::Optional(
                OptionalPageValidity::new(page),
                Values::new(page),
            )),
            (Encoding::Plain, false, false) => Ok(State::Required(Required::new(page))),
            (Encoding::Plain, true, true) => Ok(State::FilteredOptional(
                FilteredOptionalPageValidity::new(page),
                Values::new(page),
            )),
            (Encoding::Plain, false, true) => {
                Ok(State::FilteredRequired(FilteredRequired::new(page)))
            }
            _ => Err(utils::not_implemented(page)),
        }
    }

    fn with_capacity(&self, capacity: usize) -> Self::DecodedState {
        (
            MutableBitmap::with_capacity(capacity),
            MutableBitmap::with_capacity(capacity),
        )
    }

    fn extend_from_state(
        &self,
        state: &mut Self::State,
        decoded: &mut Self::DecodedState,
        remaining: usize,
    ) {
        let (values, validity) = decoded;
        match state {
            State::Optional(page_validity, page_values) => extend_from_decoder(
                validity,
                page_validity,
                Some(remaining),
                values,
                &mut page_values.0,
            ),
            State::Required(page) => {
                let remaining = remaining.min(page.length - page.offset);
                values.extend_from_slice(page.values, page.offset, remaining);
                page.offset += remaining;
            }
            State::FilteredRequired(page) => {
                values.reserve(remaining);
                for item in page.values.by_ref().take(remaining) {
                    values.push(item)
                }
            }
            State::FilteredOptional(page_validity, page_values) => {
                utils::extend_from_decoder(
                    validity,
                    page_validity,
                    Some(remaining),
                    values,
                    page_values.0.by_ref(),
                );
            }
        }
    }
}

fn finish(data_type: &DataType, values: MutableBitmap, validity: MutableBitmap) -> BooleanArray {
    BooleanArray::new(data_type.clone(), values.into(), validity.into())
}

/// An iterator adapter over [`DataPages`] assumed to be encoded as boolean arrays
#[derive(Debug)]
pub struct Iter<I: DataPages> {
    iter: I,
    data_type: DataType,
    items: VecDeque<(MutableBitmap, MutableBitmap)>,
    chunk_size: usize,
}

impl<I: DataPages> Iter<I> {
    pub fn new(iter: I, data_type: DataType, chunk_size: usize) -> Self {
        Self {
            iter,
            data_type,
            items: VecDeque::new(),
            chunk_size,
        }
    }
}

impl<I: DataPages> Iterator for Iter<I> {
    type Item = Result<BooleanArray>;

    fn next(&mut self) -> Option<Self::Item> {
        let maybe_state = next(
            &mut self.iter,
            &mut self.items,
            self.chunk_size,
            &BooleanDecoder::default(),
        );
        match maybe_state {
            MaybeNext::Some(Ok((values, validity))) => {
                Some(Ok(finish(&self.data_type, values, validity)))
            }
            MaybeNext::Some(Err(e)) => Some(Err(e)),
            MaybeNext::None => None,
            MaybeNext::More => self.next(),
        }
    }
}
