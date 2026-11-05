//! Decodes native TSP operations into indexed transactional mutations.
//! The frame owner supplies rollback storage and commits the sequence.
//! Wire arity and references are checked before structural changes.

use super::{frame::Undo, *};

pub(super) fn apply_op<'a>(
    surface: &mut OmpUiSurface,
    value: &'a Value,
    journal: &mut Vec<Undo<'a>>,
    intents: &mut Vec<OmpUiIntent>,
) -> UiResult<()> {
    let args = value.as_array().ok_or(OmpUiDocumentError::Invalid("op"))?;
    let name = args
        .first()
        .and_then(Value::as_str)
        .ok_or(OmpUiDocumentError::Invalid("op name"))?;
    match name {
        "add" => {
            arity(args, 5)?;
            let id = text(args, 1)?;
            let parent = text(args, 2)?;
            let before = optional_text(args, 3)?;
            let node = structure::parse_node(&args[4], 0)?;
            if node.id != id {
                return Err(OmpUiDocumentError::Operation);
            }
            let bytes = index::validate_insert(surface, &node, parent)?;
            let position = structure::insertion_position(surface, parent, before)?;
            structure::insert(surface, parent, position, node, bytes)?;
            journal.push(Undo::Added(id));
        }
        "set" => {
            arity(args, 3)?;
            properties::set(surface, text(args, 1)?, &args[2], journal)?;
        }
        "text" => {
            arity(args, 4)?;
            properties::text(
                surface,
                text(args, 1)?,
                text(args, 2)?,
                text(args, 3)?,
                journal,
            )?;
        }
        "splice" => {
            arity(args, 5)?;
            properties::splice(
                surface,
                text(args, 1)?,
                number(args, 2)?,
                number(args, 3)?,
                text(args, 4)?,
                journal,
            )?;
        }
        "del" => {
            arity(args, 2)?;
            journal.push(Undo::Removed(structure::remove(surface, text(args, 1)?)?));
        }
        "move" => {
            arity(args, 4)?;
            let id = text(args, 1)?;
            let parent = text(args, 2)?;
            let before = optional_text(args, 3)?;
            structure::validate_move(surface, id, parent, before)?;
            if before == Some(id) {
                return Ok(());
            }
            let removed = structure::remove(surface, id)?;
            let position = structure::insertion_position(surface, parent, before)?;
            let undo = Undo::Moved {
                id,
                parent: removed.parent,
                position: removed.position,
            };
            structure::insert(surface, parent, position, removed.node, removed.bytes)?;
            journal.push(undo);
        }
        "focus" => {
            arity(args, 2)?;
            let focus = optional_text(args, 1)?;
            if let Some(id) = focus {
                index::path(surface, id)?;
            }
            journal.push(Undo::Focus(std::mem::replace(
                &mut surface.focus,
                focus.map(str::to_owned),
            )));
        }
        "suspend" | "resume" => {
            arity(args, 1)?;
            journal.push(Undo::Suspended(std::mem::replace(
                &mut surface.suspended,
                name == "suspend",
            )));
        }
        // Native `settle` is a retention hint for the terminal's node cache;
        // it changes nothing a paint could show, so only its reference is
        // checked.
        "settle" => {
            arity(args, 2)?;
            index::path(surface, text(args, 1)?)?;
        }
        "reveal" => {
            arity(args, 3)?;
            let id = text(args, 1)?;
            index::path(surface, id)?;
            let at = OmpUiRevealAt::parse(text(args, 2)?)
                .ok_or(OmpUiDocumentError::Invalid("op reveal placement"))?;
            intents.push(OmpUiIntent::Reveal {
                id: id.to_owned(),
                at,
            });
        }
        "scroll" => {
            arity(args, 3)?;
            let id = text(args, 1)?;
            index::path(surface, id)?;
            let by = OmpUiScrollBy::parse(text(args, 2)?)
                .ok_or(OmpUiDocumentError::Invalid("op scroll direction"))?;
            intents.push(OmpUiIntent::Scroll {
                id: id.to_owned(),
                by,
                repeat: 1,
            });
        }
        _ => return Err(OmpUiDocumentError::Invalid("unknown operation")),
    }
    Ok(())
}

/// Append one request, merging a repeated scroll or an identical repeat of the
/// reveal that is already pending. The merge is what lets frames coalesced
/// before a paint keep every press without growing without bound.
pub(super) fn record_intent(surface: &mut OmpUiSurface, intent: OmpUiIntent) {
    match (&intent, surface.intents.last_mut()) {
        (
            OmpUiIntent::Reveal { id, at },
            Some(OmpUiIntent::Reveal {
                id: pending,
                at: pending_at,
            }),
        ) if id.as_str() == pending.as_str() && *at == *pending_at => return,
        (
            OmpUiIntent::Scroll { id, by, repeat },
            Some(OmpUiIntent::Scroll {
                id: pending,
                by: pending_by,
                repeat: pending_repeat,
            }),
        ) if id.as_str() == pending.as_str() && *by == *pending_by => {
            *pending_repeat = (*pending_repeat).saturating_add(*repeat);
            return;
        }
        _ => {}
    }
    if surface.intents.len() == MAX_TSP_INTENTS {
        surface.intents.remove(0);
    }
    surface.intents.push(intent);
}

fn arity(args: &[Value], required: usize) -> UiResult<()> {
    if args.len() == required {
        Ok(())
    } else {
        Err(OmpUiDocumentError::Invalid("op arity"))
    }
}
fn text(args: &[Value], position: usize) -> UiResult<&str> {
    args.get(position)
        .and_then(Value::as_str)
        .ok_or(OmpUiDocumentError::Invalid("op text"))
}
fn optional_text(args: &[Value], position: usize) -> UiResult<Option<&str>> {
    match args.get(position) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        _ => Err(OmpUiDocumentError::Invalid("op reference")),
    }
}
fn number(args: &[Value], position: usize) -> UiResult<usize> {
    usize::try_from(
        args.get(position)
            .and_then(Value::as_u64)
            .ok_or(OmpUiDocumentError::Invalid("op offset"))?,
    )
    .map_err(|_| OmpUiDocumentError::Limit)
}
