//! A tiny crate to help emulators implement save states.
//!
//! Note that this is wildly unsafe, but that's okay because it's open source.

#![deny(warnings)]
#![deny(unused_import_braces, unused_qualifications, unused_extern_crates)]

use std::io::{self, Read, Write};
use std::mem;
use std::slice;

/// Copied from `std`.
// FIXME: Remove once stable Rust has this (1.6)
// (pub for convenience only)
pub fn read_exact<R: Read + ?Sized>(r: &mut R, mut buf: &mut [u8]) -> io::Result<()> {
    while !buf.is_empty() {
        match r.read(buf) {
            Ok(0) => break,
            Ok(n) => { let tmp = buf; buf = &mut tmp[n..]; }
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    if !buf.is_empty() {
        Err(io::Error::new(io::ErrorKind::Other, "failed to fill whole buffer"))
    } else {
        Ok(())
    }
}

/// Trait for types which can be serialized in a save state.
///
/// Note that this approach is a bit different from usual serialization: `restore_state` takes a
/// `&mut self` and overwrites parts of it with the saved state. This way, we neither save the
/// renderer, nor the ROM, nor the input sources, nor temporary data that is recomputed when we
/// render a new frame after loading a save state.
///
/// When implementing this trait, use an exhaustive let-binding for all fields on the `Self` type.
/// This way, any change that could potentially require a change in the `SaveState` implementation
/// to the type will cause a compilation error. By using the `impl_save_state!` macro defined
/// below, this will be done automatically and produce an error when the struct is changed.
pub trait SaveState {
    fn save_state<W: Write + ?Sized>(&self, w: &mut W) -> io::Result<()>;
    fn restore_state<R: Read + ?Sized>(&mut self, r: &mut R) -> io::Result<()>;
}

/// Declares that a type can be safely transmuted into a byte slice of same length as the type's
/// size in Bytes and vice-versa.
///
/// This is true for all types which don't impose any restrictions on their raw content, so this
/// would not be safe to implement for types like `bool` or `char`. This is also obviously unsafe
/// for non-`#[repr(C)]`-structs.
pub unsafe trait TransmuteByteSafe {}

unsafe impl TransmuteByteSafe for u8 {}
unsafe impl TransmuteByteSafe for u16 {}
unsafe impl TransmuteByteSafe for u32 {}
unsafe impl TransmuteByteSafe for u64 {}
unsafe impl TransmuteByteSafe for usize {}
unsafe impl TransmuteByteSafe for i8 {}
unsafe impl TransmuteByteSafe for i16 {}
unsafe impl TransmuteByteSafe for i32 {}
unsafe impl TransmuteByteSafe for i64 {}
unsafe impl TransmuteByteSafe for isize {}

/// Everything that can be transmuted to/from fixed-size byte slices can trivially implement
/// `SaveState`. This includes a large portion of the emulator state, since much of it consists of
/// primitive integer values.
impl<T: TransmuteByteSafe> SaveState for T {
    fn save_state<W: Write + ?Sized>(&self, w: &mut W) -> io::Result<()> {
        let bytes = unsafe {
            slice::from_raw_parts(self as *const _ as *const u8, mem::size_of::<Self>())
        };
        w.write_all(bytes)
    }

    fn restore_state<R: Read + ?Sized>(&mut self, r: &mut R) -> io::Result<()> {
        read_exact(r, unsafe {
            slice::from_raw_parts_mut(self as *mut _ as *mut u8, mem::size_of::<Self>())
        })
    }
}

/// `bool` will always be saved as a `1` or `0` byte.
impl SaveState for bool {
    fn save_state<W: Write + ?Sized>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&[if *self {1} else {0}])
    }

    fn restore_state<R: Read + ?Sized>(&mut self, r: &mut R) -> io::Result<()> {
        let mut val = [0xff];
        read_exact(r, &mut val)?;

        match val[0] {
            0 => *self = false,
            1 => *self = true,
            _ => return Err(io::Error::new(io::ErrorKind::Other, "invalid byte value for bool")),
        }
        Ok(())
    }
}

/// `Option<T>` will either save/restore `true` followed by its contents, or `false` if it's
/// `None`.
impl<T: SaveState + Default> SaveState for Option<T> {
    fn save_state<W: Write + ?Sized>(&self, w: &mut W) -> io::Result<()> {
        match *self {
            None => false.save_state(w)?,
            Some(ref t) => {
                true.save_state(w)?;
                t.save_state(w)?;
            }
        }
        Ok(())
    }

    fn restore_state<R: Read + ?Sized>(&mut self, r: &mut R) -> io::Result<()> {
        let mut val = [0xff];
        read_exact(r, &mut val)?;

        match val[0] {
            0 => *self = None,
            1 => {
                let mut t = T::default();
                t.restore_state(r)?;
                *self = Some(t);
            }
            // FIXME Ugly, but works for now
            _ => return Err(io::Error::new(io::ErrorKind::Other, "invalid byte value for option")),
        }
        Ok(())
    }
}

macro_rules! impl_fixed_size_array {
    ( $e:expr ) => { $e };
    ( $($size:tt)+ ) => { $(
        impl<T: SaveState> SaveState for [T; impl_fixed_size_array!($size)] {
            fn save_state<W: Write + ?Sized>(&self, w: &mut W) -> io::Result<()> {
                for t in self {
                    t.save_state(w)?
                }
                Ok(())
            }

            fn restore_state<R: Read + ?Sized>(&mut self, r: &mut R) -> io::Result<()> {
                // Assume `self` always has the same size
                for t in self {
                    t.restore_state(r)?;
                }
                Ok(())
            }
        }
    )+ };
}

impl_fixed_size_array!(
    0 1 2 3 4 5 6 7 8
);

/// `Vec<T>`s `SaveState` impl will read/write the `Vec`s length first, followed by its contents.
///
/// **NOTE**: When restoring a `Vec<T>`, this will allocate an arbitrary amount of memory, so don't
/// feed it with untrusted data.
impl<T: SaveState + Default> SaveState for Vec<T> {
    fn save_state<W: Write + ?Sized>(&self, w: &mut W) -> io::Result<()> {
        // Write the len first:
        self.len().save_state(w)?;
        for item in self {
            item.save_state(w)?;
        }
        Ok(())
    }

    fn restore_state<R: Read + ?Sized>(&mut self, r: &mut R) -> io::Result<()> {
        let mut len = 0usize;
        len.restore_state(r)?;
        // FIXME We should limit the size of `len`, but it's unclear what limit to impose: Some
        // emulators may want to save a few hundred MB of RAM or something like that
        self.clear();
        self.reserve(len);
        for _ in 0..len {
            let mut obj = T::default();
            obj.restore_state(r)?;
            self.push(obj);
        }
        Ok(())
    }
}

/// Generates the functions of the `SaveState` trait.
///
/// The `impl_save_state` macro should be the preferred way of implementing the `SaveState` trait.
/// However, that might not be possible. For example, the type for which to implement `SaveState`
/// might have generic parameters, which must be reflected on all `impl`s. In that case, you can
/// manually write an `impl` block and use this macro in its body to generate the actual
/// implementation.
///
/// # Example
///
/// ```
/// #[macro_use]
/// extern crate libsavestate;
///
/// struct Generic<T> {
///     field: T,
///     other: u8,
/// }
///
/// // Doesn't work, even though `field` is ignored :(
/// //impl_save_state!(Generic {
/// //    other
/// //} ignore {
/// //    field
/// //});
///
/// // This works, but we need the right `impl` block around it
/// impl<T> ::libsavestate::SaveState for Generic<T> {
///     impl_save_state_fns!(Generic {
///         other
///     } ignore {
///         field
///     });
/// }
///
/// # fn main() {}
/// ```
#[macro_export]
macro_rules! impl_save_state_fns {
    ( $t:ident { $( $field:ident ),* } ignore { $( $ignore:ident ),* } ) => {
        fn save_state<W: ::std::io::Write + ?Sized>(&self, w: &mut W) -> ::std::io::Result<()> {
            let $t { $(ref $field,)* $(ref $ignore,)* } = *self;
            $(
                try!($field.save_state(w));
            );*
            $(
                let _ = $ignore;
            )*
            Ok(())
        }

        fn restore_state<R: ::std::io::Read + ?Sized>(&mut self, r: &mut R) -> ::std::io::Result<()> {
            let $t { $(ref mut $field,)* $(ref mut $ignore,)* } = *self;
            $(
                try!($field.restore_state(r));
            );*
            $(
                let _ = $ignore;
            )*
            Ok(())
        }
    };
}

/// Generates an impl of `SaveState` for a given type, saving/restoring a list of fields, and
/// ignoring a second list of fields.
///
/// To prevent bugs, these lists must contains all fields of the type exactly once or this won't
/// compile.
///
/// # Example
/// ```
/// #[macro_use]
/// extern crate libsavestate;
///
/// struct Struct {
///     field: u8,
///     field2: Vec<u8>,
///     ignored: String,
/// }
///
/// impl_save_state!(Struct {
///     field, field2
/// } ignore {
///     ignored
/// });
///
/// # fn main() {}
/// ```
#[macro_export]
macro_rules! impl_save_state {
    ( $t:ident { $( $field:ident ),* } ignore { $( $ignore:ident ),* } ) => {
        impl $crate::SaveState for $t {
            impl_save_state_fns!($t { $( $field ),* } ignore { $( $ignore ),* });
        }
    };
}

/// Generates a `SaveState` impl for a newtype wrapper. The type must be declared as
/// `SomeType(InnerType)` and `InnerType` must implement `SaveState`.
// FIXME Should we merge this into the other macro or is it better if it's explicit?
#[macro_export]
macro_rules! impl_save_state_for_newtype {
    ( $t:ident ) => {
        impl $crate::SaveState for $t {
            fn save_state<W: ::std::io::Write + ?Sized>(&self, w: &mut W) -> ::std::io::Result<()> {
                self.0.save_state(w)
            }

            fn restore_state<R: ::std::io::Read + ?Sized>(&mut self, r: &mut R) -> ::std::io::Result<()> {
                self.0.restore_state(r)
            }
        }
    };
}
