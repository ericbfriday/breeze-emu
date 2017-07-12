#![deny(warnings)]
#![deny(unused_import_braces, unused_qualifications, unused_extern_crates)]

#[macro_use] extern crate log;
extern crate byteorder;
extern crate slicevec;

#[macro_use] #[no_link] extern crate byte_array;
#[macro_use] extern crate libsavestate;
extern crate wdc65816;
extern crate spc700;
extern crate breeze_backend;

#[macro_use] mod log_util;
pub mod dma;
pub mod record;
pub mod ppu;
pub mod input;
pub mod rom;
pub mod save;
pub mod snes;
