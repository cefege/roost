//! Conversions between an OSC 7501 record and its generated protobuf form,
//! the one `ProgramStatusRecord` message the worker's terminal metadata and the
//! coordinator's terminal-signals frame both carry. The state and kind codes
//! live on `ProgramState` and `ProgramKind`; this module maps fields only.

use roost_proto::ProgramStatusRecord as PbProgramStatusRecord;

use crate::program_status::{ProgramKind, ProgramState, ProgramStatusRecord};
use crate::{ProtocolError, ProtocolResult};

pub fn program_status_to_proto(record: &ProgramStatusRecord) -> PbProgramStatusRecord {
    PbProgramStatusRecord {
        id: record.id.clone(),
        state: record.state.as_wire().to_string(),
        kind: record.kind.map(|kind| kind.as_wire().to_string()),
        app: record.app.clone(),
        title: record.title.clone(),
        msg: record.msg.clone(),
        progress: record.progress.map(u32::from),
        ..Default::default()
    }
}

pub fn program_status_records_to_proto(
    records: &[ProgramStatusRecord],
) -> Vec<PbProgramStatusRecord> {
    records.iter().map(program_status_to_proto).collect()
}

pub fn program_status_from_proto(
    record: &PbProgramStatusRecord,
) -> ProtocolResult<ProgramStatusRecord> {
    let state = ProgramState::from_wire(&record.state).ok_or_else(|| {
        ProtocolError::new(
            "program_status.state",
            format!("{} is not an OSC 7501 state", record.state),
        )
    })?;
    let kind = record
        .kind
        .as_deref()
        .map(|wire| {
            ProgramKind::from_wire(wire).ok_or_else(|| {
                ProtocolError::new(
                    "program_status.kind",
                    format!("{wire} is not an OSC 7501 kind"),
                )
            })
        })
        .transpose()?;
    let progress = record
        .progress
        .map(|percent| {
            u8::try_from(percent)
                .ok()
                .filter(|percent| *percent <= 100)
                .ok_or_else(|| {
                    ProtocolError::new(
                        "program_status.progress",
                        format!("{percent} is not a 0-100 progress percent"),
                    )
                })
        })
        .transpose()?;
    Ok(ProgramStatusRecord {
        id: record.id.clone(),
        state,
        kind,
        app: record.app.clone(),
        title: record.title.clone(),
        msg: record.msg.clone(),
        progress,
    })
}

pub fn program_status_records_from_proto(
    records: &[PbProgramStatusRecord],
) -> ProtocolResult<Vec<ProgramStatusRecord>> {
    records.iter().map(program_status_from_proto).collect()
}
