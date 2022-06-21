//! GraphQL implementation for [array].
//!
//! [array]: prim@array

use std::{
    mem::{self, MaybeUninit},
    ptr,
};

use crate::{
    behavior,
    executor::{ExecutionResult, Executor, Registry},
    graphql, reflect, resolve,
    schema::meta::MetaType,
    BoxFuture, FieldError, IntoFieldError, Selection,
};

use super::iter;

impl<T, TI, SV, BH, const N: usize> resolve::Type<TI, SV, BH> for [T; N]
where
    T: resolve::Type<TI, SV, BH>,
    TI: ?Sized,
    BH: ?Sized,
{
    fn meta<'r, 'ti: 'r>(registry: &mut Registry<'r, SV>, type_info: &'ti TI) -> MetaType<'r, SV>
    where
        SV: 'r,
    {
        registry.wrap_list::<behavior::Coerce<T, BH>, _>(type_info, None)
    }
}

impl<T, TI, CX, SV, BH, const N: usize> resolve::Value<TI, CX, SV, BH> for [T; N]
where
    T: resolve::Value<TI, CX, SV, BH>,
    TI: ?Sized,
    CX: ?Sized,
    BH: ?Sized,
{
    fn resolve_value(
        &self,
        selection_set: Option<&[Selection<'_, SV>]>,
        type_info: &TI,
        executor: &Executor<CX, SV>,
    ) -> ExecutionResult<SV> {
        iter::resolve_list(self.iter(), selection_set, type_info, executor)
    }
}

impl<T, TI, CX, SV, BH, const N: usize> resolve::ValueAsync<TI, CX, SV, BH> for [T; N]
where
    T: resolve::ValueAsync<TI, CX, SV, BH> + Sync,
    TI: Sync + ?Sized,
    CX: Sync + ?Sized,
    SV: Send + Sync,
    BH: ?Sized + 'static, // TODO: Lift `'static` bound if possible.
{
    fn resolve_value_async<'r>(
        &'r self,
        selection_set: Option<&'r [Selection<'_, SV>]>,
        type_info: &'r TI,
        executor: &'r Executor<CX, SV>,
    ) -> BoxFuture<'r, ExecutionResult<SV>> {
        Box::pin(iter::resolve_list_async(
            self.iter(),
            selection_set,
            type_info,
            executor,
        ))
    }
}

impl<T, SV, BH, const N: usize> resolve::ToInputValue<SV, BH> for [T; N]
where
    T: resolve::ToInputValue<SV, BH>,
    BH: ?Sized,
{
    fn to_input_value(&self) -> graphql::InputValue<SV> {
        graphql::InputValue::list(self.iter().map(T::to_input_value))
    }
}

impl<'i, T, SV, BH, const N: usize> resolve::InputValue<'i, SV, BH> for [T; N]
where
    T: resolve::InputValue<'i, SV, BH>,
    SV: 'i,
    BH: ?Sized,
{
    type Error = TryFromInputValueError<T::Error>;

    fn try_from_input_value(v: &'i graphql::InputValue<SV>) -> Result<Self, Self::Error> {
        struct PartiallyInitializedArray<T, const N: usize> {
            arr: [MaybeUninit<T>; N],
            init_len: usize,
            no_drop: bool,
        }

        impl<T, const N: usize> Drop for PartiallyInitializedArray<T, N> {
            fn drop(&mut self) {
                if self.no_drop {
                    return;
                }
                // Dropping a `MaybeUninit` does nothing, thus we need to drop
                // the initialized elements manually, otherwise we may introduce
                // a memory/resource leak if `T: Drop`.
                for elem in &mut self.arr[0..self.init_len] {
                    // SAFETY: This is safe, because `self.init_len` represents
                    //         the number of the initialized elements exactly.
                    unsafe {
                        ptr::drop_in_place(elem.as_mut_ptr());
                    }
                }
            }
        }

        match v {
            graphql::InputValue::List(ls) => {
                if ls.len() != N {
                    return Err(TryFromInputValueError::WrongCount {
                        actual: ls.len(),
                        expected: N,
                    });
                }
                if N == 0 {
                    // TODO: Use `mem::transmute` instead of
                    //       `mem::transmute_copy` below, once it's allowed
                    //       for const generics:
                    //       https://github.com/rust-lang/rust/issues/61956
                    // SAFETY: `mem::transmute_copy` is safe here, because we
                    //         check `N` to be `0`. It's no-op, actually.
                    return Ok(unsafe { mem::transmute_copy::<[T; 0], Self>(&[]) });
                }

                // SAFETY: The reason we're using a wrapper struct implementing
                //         `Drop` here is to be panic safe:
                //         `T: resolve::InputValue` implementation is not
                //         controlled by us, so calling
                //         `T::try_from_input_value(&i.item)` below may cause a
                //         panic when our array is initialized only partially.
                //         In such situation we need to drop already initialized
                //         values to avoid possible memory/resource leaks if
                //         `T: Drop`.
                let mut out = PartiallyInitializedArray::<T, N> {
                    // SAFETY: The `.assume_init()` here is safe, because the
                    //         type we are claiming to have initialized here is
                    //         a bunch of `MaybeUninit`s, which do not require
                    //         any initialization.
                    arr: unsafe { MaybeUninit::uninit().assume_init() },
                    init_len: 0,
                    no_drop: false,
                };

                let mut items = ls.iter().map(|i| T::try_from_input_value(&i.item));
                for elem in &mut out.arr[..] {
                    if let Some(i) = items
                        .next()
                        .transpose()
                        .map_err(TryFromInputValueError::Item)?
                    {
                        *elem = MaybeUninit::new(i);
                        out.init_len += 1;
                    }
                }

                // Do not drop collected `items`, because we're going to return
                // them.
                out.no_drop = true;

                // TODO: Use `mem::transmute` instead of `mem::transmute_copy`
                //       below, once it's allowed for const generics:
                //       https://github.com/rust-lang/rust/issues/61956
                // SAFETY: `mem::transmute_copy` is safe here, because we have
                //         exactly `N` initialized `items`.
                //         Also, despite `mem::transmute_copy` copies the value,
                //         we won't have a double-free when `T: Drop` here,
                //         because original array elements are `MaybeUninit`, so
                //         do nothing on `Drop`.
                Ok(unsafe { mem::transmute_copy::<_, Self>(&out.arr) })
            }
            // See "Input Coercion" on List types:
            // https://spec.graphql.org/October2021#sec-Combining-List-and-Non-Null
            graphql::InputValue::Null => Err(TryFromInputValueError::IsNull),
            other => T::try_from_input_value(other)
                .map_err(TryFromInputValueError::Item)
                .and_then(|e: T| {
                    // TODO: Use `mem::transmute` instead of
                    //       `mem::transmute_copy` below, once it's allowed
                    //       for const generics:
                    //       https://github.com/rust-lang/rust/issues/61956
                    if N == 1 {
                        // SAFETY: `mem::transmute_copy` is safe here, because
                        //         we check `N` to be `1`. Also, despite
                        //         `mem::transmute_copy` copies the value, we
                        //         won't have a double-free when `T: Drop` here,
                        //         because original `e: T` value is wrapped into
                        //         `mem::ManuallyDrop`, so does nothing on
                        //         `Drop`.
                        Ok(unsafe { mem::transmute_copy::<_, Self>(&[mem::ManuallyDrop::new(e)]) })
                    } else {
                        Err(TryFromInputValueError::WrongCount {
                            actual: 1,
                            expected: N,
                        })
                    }
                }),
        }
    }
}

impl<'i, T, TI, SV, BH, const N: usize> graphql::InputType<'i, TI, SV, BH> for [T; N]
where
    T: graphql::InputType<'i, TI, SV, BH>,
    TI: ?Sized,
    SV: 'i,
    BH: ?Sized,
{
    fn assert_input_type() {
        T::assert_input_type()
    }
}

impl<T, TI, CX, SV, BH, const N: usize> graphql::OutputType<TI, CX, SV, BH> for [T; N]
where
    T: graphql::OutputType<TI, CX, SV, BH>,
    TI: ?Sized,
    CX: ?Sized,
    BH: ?Sized,
    Self: resolve::ValueAsync<TI, CX, SV, BH>,
{
    fn assert_output_type() {
        T::assert_output_type()
    }
}

impl<T, BH, const N: usize> reflect::BaseType<BH> for [T; N]
where
    T: reflect::BaseType<BH>,
    BH: ?Sized,
{
    const NAME: reflect::Type = T::NAME;
}

impl<T, BH, const N: usize> reflect::BaseSubTypes<BH> for [T; N]
where
    T: reflect::BaseSubTypes<BH>,
    BH: ?Sized,
{
    const NAMES: reflect::Types = T::NAMES;
}

impl<T, BH, const N: usize> reflect::WrappedType<BH> for [T; N]
where
    T: reflect::WrappedType<BH>,
    BH: ?Sized,
{
    const VALUE: reflect::WrappedValue = reflect::wrap::list(T::VALUE);
}

/// Possible errors of converting a [`graphql::InputValue`] into an exact-size
/// [array](prim@array).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TryFromInputValueError<E> {
    /// [`graphql::InputValue`] cannot be [`Null`].
    ///
    /// See ["Combining List and Non-Null" section of spec][0].
    ///
    /// [`Null`]: [`InputValue::Null`]
    /// [0]: https://spec.graphql.org/October2021#sec-Combining-List-and-Non-Null
    IsNull,

    /// Wrong count of items.
    WrongCount {
        /// Actual count of items.
        actual: usize,

        /// Expected count of items ([array](prim@array) size).
        expected: usize,
    },

    /// Error of converting a [`graphql::InputValue::List`]'s item.
    Item(E),
}

impl<E, SV> IntoFieldError<SV> for TryFromInputValueError<E>
where
    E: IntoFieldError<SV>,
{
    fn into_field_error(self) -> FieldError<SV> {
        const ERROR_PREFIX: &str = "Failed to convert into exact-size array";
        match self {
            Self::IsNull => format!("{}: Value cannot be `null`", ERROR_PREFIX).into(),
            Self::WrongCount { actual, expected } => format!(
                "{}: wrong elements count: {} instead of {}",
                ERROR_PREFIX, actual, expected,
            )
            .into(),
            Self::Item(s) => s.into_field_error(),
        }
    }
}