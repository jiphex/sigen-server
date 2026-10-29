//! Blocking Sigenergy Modbus TCP client based on the manufacturer's V2.7 specification.
//!
//! See the crate README for the source, supported registers and function-code ambiguity.
//! Use one client per plant to preserve request pacing. In async applications, run this
//! client on a blocking worker. No request is automatically retried (especially writes).

#![doc = include_str!("../README.md")]

mod client;
mod data;

pub use client::{Client, Config, Error, ReadFunction, Result, UnitId, WriteAuditor, WriteRecord};
pub use data::*;

/// Standard Modbus TCP port (specification §3.2).
pub const DEFAULT_PORT: u16 = 502;
/// Manufacturer document used for register addresses and engineering units.
pub const SPECIFICATION_URL: &str =
    "https://www.sigenergy.com/uploads/us_download/1755488219226583.pdf";
