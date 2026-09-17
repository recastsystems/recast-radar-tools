//! NEXRAD Level II real-time chunks: chunk timing model and WSR-88D VCP
//! catalog, retry policy, pull-based chunk iterator and (feature `async`)
//! chunk stream.

pub mod iterator;
pub mod retry;
#[cfg(feature = "async")]
pub mod stream;
pub mod timing;
pub mod vcp_catalog;
