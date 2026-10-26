//! The Fleet destination: a card per registered machine with live readings while
//! the page is open. Pure readings and subscription diffs sit beside the
//! rendering so their logic is unit-tested on the native target.

pub mod fleet_page;
pub mod machine_card;
pub mod process_table;
pub mod readings;
pub mod subscription;

pub use fleet_page::FleetPage;
