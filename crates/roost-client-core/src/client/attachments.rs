//! One browser file's upload, from the grant that authorises it to the card
//! that reports it. `direct` picks the carrier, `transfer` chunks and settles,
//! `conversation` holds the frame rules both carriers share, and `insertion`
//! decides whether the resulting path may be typed into a PTY. Ported from
//! `apps/web/src/client/attachments/`, which is the oracle. No socket, no file
//! and no timer: a host performs every act and reports what it observed.

pub mod conversation;
pub mod direct;
pub mod grant;
pub mod insertion;
pub mod packets;
pub mod peer;
pub mod signaling;
pub mod transfer;
