//! Shared terminal-view geometry and lease policy.
//!
//! The coordinator's terminal view hub is the only membership owner; browsers
//! clamp trusted measurements to these core limits and every later trust boundary
//! rejects an out-of-range value. `minimum_terminal_geometry` is the ONLY
//! per-axis aggregation in the workspace: a hand-rolled minimum beside this
//! primitive is how a session stays clipped to a viewer nobody is looking at.

use serde::{Deserialize, Serialize};

use crate::error::{ProtocolError, ProtocolResult};

/// The widest and tallest grid a session may be sized to.
pub const TERMINAL_MAX_COLS: u32 = 256;
pub const TERMINAL_MAX_ROWS: u32 = 256;

/// How long a claimed view stays claimable before the sweeper reclaims it.
pub const TERMINAL_VIEW_LEASE_MS: u64 = 15_000;
/// How often a live view refreshes its claim.
pub const TERMINAL_VIEW_HEARTBEAT_MS: u64 = 5_000;
/// How often the sweeper looks for expired claims.
pub const TERMINAL_VIEW_SWEEP_MS: u64 = 1_000;
/// How long a parked view keeps constraining effective geometry after its socket
/// dropped. Much shorter than the lease because reclaim and geometry are
/// different questions: the record stays claimable for TERMINAL_VIEW_LEASE_MS,
/// but a viewer nobody is looking at stops shrinking everyone else's PTY.
pub const TERMINAL_VIEW_PARK_GRACE_MS: u64 = 2_000;
/// How long a foreground view waits before re-probing an idle session's liveness.
pub const TERMINAL_FOREGROUND_IDLE_PROBE_MS: u64 = 5_000;
/// How long a foreground probe may take before the answer is treated as no answer.
pub const TERMINAL_FOREGROUND_PROBE_DEADLINE_MS: u64 = 3_000;
/// Hard product cap for distinct terminal sessions viewed from one socket.
pub const TERMINAL_SOCKET_VIEW_CAP: usize = 64;

/// A grid size, on both axes independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TerminalGeometry {
    pub cols: u32,
    pub rows: u32,
}

/// 8-4-4-4-12 hex with dashes: the shape `is_terminal_uuid` accepts.
const UUID_LEN: usize = 36;

/// The UUID shape used by view, stream and snapshot generations. IDs are opaque;
/// accepting every RFC-4122 version (1 through 8) and either case keeps
/// validation independent of how a caller obtained its collision-resistant UUID.
pub fn is_terminal_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != UUID_LEN {
        return false;
    }
    for (position, byte) in bytes.iter().enumerate() {
        if matches!(position, 8 | 13 | 18 | 23) {
            if *byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    // The dashes above guarantee both nibbles exist in a 36-byte string: index 14
    // is the version and 19 the variant.
    matches!(bytes[14], b'1'..=b'8') && matches!(bytes[19], b'8' | b'9' | b'a' | b'A' | b'b' | b'B')
}

/// Whether a geometry is inside the core limits on both axes.
pub fn is_terminal_geometry(value: &TerminalGeometry) -> bool {
    value.cols >= 1
        && value.cols <= TERMINAL_MAX_COLS
        && value.rows >= 1
        && value.rows <= TERMINAL_MAX_ROWS
}

/// Validate untrusted wire/API geometry without mutating it.
pub fn assert_terminal_geometry(value: &TerminalGeometry) -> ProtocolResult<TerminalGeometry> {
    if !is_terminal_geometry(value) {
        return Err(ProtocolError::new(
            "terminal_geometry",
            format!(
                "terminal geometry {}x{} is outside 1..{TERMINAL_MAX_COLS}x1..{TERMINAL_MAX_ROWS}",
                value.cols, value.rows
            ),
        ));
    }
    Ok(*value)
}

/// Bound a trusted browser measurement before it enters the wire.
///
/// The measurement is a float and the wire type is integral, so this is the one
/// place the two meet: a non-finite or sub-cell measurement collapses to the
/// minimum, a fractional one floors, and anything past the core limits clamps.
/// A caller already holding a `TerminalGeometry` has nothing to clamp.
pub fn clamp_terminal_geometry(cols: f64, rows: f64) -> TerminalGeometry {
    TerminalGeometry {
        cols: clamp_axis(cols, TERMINAL_MAX_COLS),
        rows: clamp_axis(rows, TERMINAL_MAX_ROWS),
    }
}

/// The independent-axis smallest-common dimensions of the views that are
/// actually constraining a session. Empty input means unwatched: the last
/// effective geometry is held, never re-minted, so a flapping link cannot become
/// a stream re-mint storm.
pub fn minimum_terminal_geometry<'a>(
    geometries: impl IntoIterator<Item = &'a TerminalGeometry>,
) -> ProtocolResult<Option<TerminalGeometry>> {
    let mut cols = TERMINAL_MAX_COLS + 1;
    let mut rows = TERMINAL_MAX_ROWS + 1;
    let mut seen = false;
    for geometry in geometries {
        assert_terminal_geometry(geometry)?;
        cols = cols.min(geometry.cols);
        rows = rows.min(geometry.rows);
        seen = true;
    }
    Ok(seen.then_some(TerminalGeometry { cols, rows }))
}

fn clamp_axis(measured: f64, max: u32) -> u32 {
    if !measured.is_finite() {
        return 1;
    }
    measured.floor().clamp(1.0, f64::from(max)) as u32
}

#[cfg(test)]
mod tests {
    use super::{
        TERMINAL_MAX_COLS, TERMINAL_MAX_ROWS, TerminalGeometry, assert_terminal_geometry,
        clamp_terminal_geometry, is_terminal_geometry, is_terminal_uuid, minimum_terminal_geometry,
    };

    fn geometry(cols: u32, rows: u32) -> TerminalGeometry {
        TerminalGeometry { cols, rows }
    }

    fn uuid(version: char, variant: char) -> String {
        format!("00000000-0000-{version}000-{variant}000-000000000001")
    }

    #[test]
    fn geometry_is_in_range_at_both_corners_and_nowhere_else() {
        assert!(is_terminal_geometry(&geometry(1, 1)));
        assert!(is_terminal_geometry(&geometry(
            TERMINAL_MAX_COLS,
            TERMINAL_MAX_ROWS
        )));
        assert!(!is_terminal_geometry(&geometry(0, 1)));
        assert!(!is_terminal_geometry(&geometry(1, 0)));
        assert!(!is_terminal_geometry(&geometry(TERMINAL_MAX_COLS + 1, 1)));
        assert!(!is_terminal_geometry(&geometry(1, TERMINAL_MAX_ROWS + 1)));
    }

    #[test]
    fn an_out_of_range_geometry_names_the_pair_it_refused() {
        let fault = match assert_terminal_geometry(&geometry(0, 24)) {
            Err(fault) => fault,
            Ok(_) => panic!("a zero-width grid must be refused"),
        };
        assert_eq!(fault.field, "terminal_geometry");
        assert!(
            fault.reason.contains("0x24 is outside 1..256x1..256"),
            "{}",
            fault.reason
        );
    }

    #[test]
    fn every_rfc_4122_version_and_both_variant_cases_decode() {
        for version in ['1', '4', '8'] {
            assert!(is_terminal_uuid(&uuid(version, '8')));
        }
        for variant in ['8', '9', 'a', 'b', 'A', 'B'] {
            assert!(is_terminal_uuid(&uuid('4', variant)));
        }
        assert!(is_terminal_uuid("00000000-0000-4000-8000-00000000010A"));
        // Out of shape: wrong length, a missing dash, a non-hex digit, a version
        // outside 1..8, and a variant outside 8/9/a/b.
        assert!(!is_terminal_uuid("00000000-0000-4000-8000-00000000001"));
        assert!(!is_terminal_uuid("000000000000-4000-8000-000000000001"));
        assert!(!is_terminal_uuid("00000000-0000-4000-8000-00000000000g"));
        assert!(!is_terminal_uuid("00000000-0000-0000-8000-000000000001"));
        assert!(!is_terminal_uuid("00000000-0000-9000-8000-000000000001"));
        assert!(!is_terminal_uuid("00000000-0000-4000-c000-000000000001"));
        // A trailing newline is not a UUID here even though a JavaScript
        // regular expression's `$` would have accepted one.
        assert!(!is_terminal_uuid("00000000-0000-4000-8000-000000000001\n"));
    }

    #[test]
    fn the_minimum_is_taken_on_each_axis_independently() {
        let wide_and_short = [geometry(100, 10), geometry(80, 20)];
        assert_eq!(
            minimum_terminal_geometry(&wide_and_short),
            Ok(Some(geometry(80, 10)))
        );
        // Empty input is unwatched, not "the maximum".
        let unwatched: [TerminalGeometry; 0] = [];
        assert_eq!(minimum_terminal_geometry(&unwatched), Ok(None));
        // One axis alone still yields both axes.
        assert_eq!(
            minimum_terminal_geometry(&[geometry(120, 40)]),
            Ok(Some(geometry(120, 40)))
        );
    }

    #[test]
    fn the_minimum_refuses_an_input_it_could_not_aggregate() {
        // A view that carries no live claim must never shrink the session, and
        // this primitive is what decides that — so it refuses rather than
        // silently skipping the record.
        assert!(minimum_terminal_geometry(&[geometry(80, 24), geometry(0, 0)]).is_err());
    }

    #[test]
    fn a_measurement_is_floored_and_clamped_on_both_axes() {
        // Below the minimum.
        assert_eq!(clamp_terminal_geometry(0.0, 0.0), geometry(1, 1));
        assert_eq!(clamp_terminal_geometry(-40.0, 24.0), geometry(1, 24));
        // A sub-cell measurement is not a cell.
        assert_eq!(clamp_terminal_geometry(0.4, 24.9), geometry(1, 24));
        // In range.
        assert_eq!(clamp_terminal_geometry(80.0, 24.0), geometry(80, 24));
        assert_eq!(clamp_terminal_geometry(80.9, 24.0), geometry(80, 24));
        // Above the maximum.
        assert_eq!(
            clamp_terminal_geometry(1_000.0, 1_000.0),
            geometry(256, 256)
        );
        assert_eq!(clamp_terminal_geometry(256.0, 257.0), geometry(256, 256));
        // A measurement that never resolved reads as the minimum, not as a hole.
        assert_eq!(clamp_terminal_geometry(f64::NAN, 24.0), geometry(1, 24));
        assert_eq!(
            clamp_terminal_geometry(80.0, f64::INFINITY),
            geometry(80, 1)
        );
    }
}
