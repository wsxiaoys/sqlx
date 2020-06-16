//! Provides [`Decode`](trait.Decode.html) for decoding values from the database.

use crate::database::{Database, HasValueRef};
use crate::error::BoxDynError;
use crate::types::Type;
use crate::value::ValueRef;

/// A type that can be decoded from the database.
///
/// ## Derivable
///
/// This trait can be derived to provide user-defined types where supported by
/// the database driver; or, to provide transparent Rust type wrappers over defined SQL types.
///
/// ```rust,ignore
/// // `UserId` can now be decoded from the database where
/// // an `i64` was expected.
/// #[derive(Decode)]
/// struct UserId(i64);
/// ```
///
/// ## How can I implement `Decode`?
///
/// A manual implementation of `Decode` can be useful when adding support for
/// types externally to SQLx.
///
/// The following showcases how to implement `Decode` to be generic over `Database`. The
/// implementation can be marginally simpler if you remove the `DB` type parameter and explicitly
/// use the concrete `ValueRef` and `TypeInfo` types.
///
/// ```rust
/// # use sqlx_core::database::{Database, HasValueRef};
/// # use sqlx_core::decode::Decode;
/// # use sqlx_core::types::Type;
/// # use std::error::Error;
/// #
/// struct MyType;
///
/// # impl<DB: Database> Type<DB> for MyType {
/// # fn type_info() -> DB::TypeInfo { todo!() }
/// # }
/// #
/// # impl std::str::FromStr for MyType {
/// # type Err = sqlx_core::error::Error;
/// # fn from_str(s: &str) -> Result<Self, Self::Err> { todo!() }
/// # }
/// #
/// // DB is the database driver
/// // `'r` is the lifetime of the `Row` being decoded
/// impl<'r, DB: Database> Decode<'r, DB> for MyType
/// where
///     // we want to delegate some of the work to string decoding so let's make sure strings
///     // are supported by the database
///     &'r str: Decode<'r, DB>
/// {
///     fn accepts(ty: &DB::TypeInfo) -> bool {
///         // accepts is intended to provide runtime type checking and assert that our decode
///         // function can handle the incoming value from the database
///
///         // as we are delegating to String
///         <&str as Decode<DB>>::accepts(ty)
///     }
///
///     fn decode(
///         value: <DB as HasValueRef<'r>>::ValueRef,
///     ) -> Result<MyType, Box<dyn Error + 'static + Send + Sync>> {
///         // the interface of ValueRef is largely unstable at the moment
///         // so this is not directly implementable
///
///         // however, you can delegate to a type that matches the format of the type you want
///         // to decode (such as a UTF-8 string)
///
///         let value = <&str as Decode<DB>>::decode(value)?;
///
///         // now you can parse this into your type (assuming there is a `FromStr`)
///
///         Ok(value.parse()?)
///     }
/// }
/// ```
pub trait Decode<'r, DB: Database>: Sized {
    /// Determines if a value of this type can be created from a value with the
    /// given type information.
    fn accepts(ty: &DB::TypeInfo) -> bool;

    /// Decode a new value of this type using a raw value from the database.
    fn decode(value: <DB as HasValueRef<'r>>::ValueRef) -> Result<Self, BoxDynError>;
}

// implement `Decode` for Option<T> for all SQL types
impl<'r, DB, T> Decode<'r, DB> for Option<T>
where
    DB: Database,
    T: Decode<'r, DB>,
{
    fn accepts(ty: &DB::TypeInfo) -> bool {
        T::accepts(ty)
    }

    fn decode(value: <DB as HasValueRef<'r>>::ValueRef) -> Result<Self, BoxDynError> {
        if value.is_null() {
            Ok(None)
        } else {
            Ok(Some(T::decode(value)?))
        }
    }
}

// default implementation of `accepts`
// this can be trivially removed once min_specialization is stable
#[allow(dead_code)]
pub(crate) fn accepts<DB: Database, T: Type<DB>>(ty: &DB::TypeInfo) -> bool {
    *ty == T::type_info()
}
