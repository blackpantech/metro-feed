//! Network access over HTTP.
//!
//! - [`client`]: the [`client::Fetcher`] transport (conditional GET,
//!   timeouts, body cap, redirect limit). Parsing is delegated to
//!   [`crate::parse`].
//! - [`discovery`]: URL autodiscovery for `metro feeds add`, built on the
//!   same client.
//!
//! Nothing here touches the database; recording outcomes on feed rows is the
//! job of [`crate::feed_sync`].

pub mod client;
pub mod discovery;
