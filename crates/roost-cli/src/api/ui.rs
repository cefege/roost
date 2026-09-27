//! `roost api ui-state` and the `ui` command family: what a paired browser tab
//! last reported, and the commands that drive one. Called by `api::mod`;
//! depends on the generated `Ui*` methods and on `api::client`.
//!
//! WHAT A `ui` COMMAND REPORTS IS A COUNT, NOT A CONFIRMATION. The dispatch
//! answers with the number of sync subscribers a command reached, and that
//! number is an upper bound: a stale tab holds a stream too. So the count is
//! printed as a count, and a caller that needs to know a command ran waits for
//! the effect it asked for rather than trusting a non-zero subscriber count.
//!
//! WHY `--tab` IS EMPTY BY DEFAULT. An empty target tab id means every tab,
//! which is the right answer for "arrange the windows" and the wrong one for
//! "close this tab". Naming a tab is therefore always available and never
//! required, and `close-tab` is the case where getting it wrong costs
//! something, so it requires one.

use std::process::ExitCode;

use roost_proto::__buffa::oneof::ui_command::Command;
use roost_proto::{
    UiArrange, UiCloseTab, UiCommand, UiDispatchRequest, UiFocusPane, UiListStatesRequest,
    UiMoveTab, UiNavigate, UiPlaceSplit, UiSelectTab, UiSpotlight,
};

use crate::api::client::CoordinatorApi;
use crate::api::output::ApiOutput;
use crate::api::verbs::Invocation;
use crate::command_error::CommandFailure;

/// The presets `ui arrange` accepts, as `roost-protocol`'s layout module names
/// them.
const ARRANGE_PRESETS: [&str; 5] = ["even", "rows", "tiled", "main-vertical", "balance"];

/// Every tab that has reported state, one block per tab.
pub async fn state(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let response = api
        .answer(api.stub().ui_list_states(UiListStatesRequest::default()))
        .await?;
    out.answer("fp\ttab\tlabel\tlast_ms");
    for tab in &response.tabs {
        out.answer(&format!(
            "{}\t{}\t{}\t{}",
            tab.fp,
            tab.tab_id,
            if tab.label.is_empty() {
                "-"
            } else {
                &tab.label
            },
            tab.last_ms
        ));
    }
    Ok(ExitCode::SUCCESS)
}

/// Drive one paired browser tab.
pub async fn command(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let subcommand = args.positional(0, "ui command")?;
    let target = args.optional_value("--tab").unwrap_or_default().to_string();
    let session = |index: usize| -> Result<String, CommandFailure> {
        args.positional(index, "session").map(str::to_string)
    };
    let command = match subcommand {
        "navigate" => Command::Navigate(Box::new(UiNavigate {
            path: args.positional(1, "path")?.to_string(),
            ..Default::default()
        })),
        "place-split" => {
            // positionals: 0 the subcommand, 1 the session, 2 its anchor, 3 the
            // direction. The direction is read last because a caller who
            // omitted it should hear about the direction, not about arity.
            let direction = args.positional(3, "row|col")?;
            if direction != "row" && direction != "col" {
                return Err(CommandFailure::usage(format!(
                    "roost api ui place-split: the direction must be row or col, got \
                     {direction:?}"
                )));
            }
            Command::PlaceSplit(Box::new(UiPlaceSplit {
                session_id: session(1)?,
                anchor_session_id: session(2)?,
                dir: direction.to_string(),
                insert_first: args.has("--first"),
                ..Default::default()
            }))
        }
        "select-tab" => Command::SelectTab(Box::new(UiSelectTab {
            session_id: session(1)?,
            ..Default::default()
        })),
        "focus-pane" => Command::FocusPane(Box::new(UiFocusPane {
            session_id: session(1)?,
            ..Default::default()
        })),
        "move-tab" => Command::MoveTab(Box::new(UiMoveTab {
            session_id: session(1)?,
            dest_session_id: session(2)?,
            ..Default::default()
        })),
        "arrange" => {
            let preset = args.positional(1, "preset")?;
            if !ARRANGE_PRESETS.contains(&preset) {
                return Err(CommandFailure::usage(format!(
                    "roost api ui arrange: the preset must be one of {}, got {preset:?}",
                    ARRANGE_PRESETS.join("|")
                )));
            }
            Command::Arrange(Box::new(UiArrange {
                preset: preset.to_string(),
                ..Default::default()
            }))
        }
        "close-tab" => {
            if target.is_empty() {
                return Err(CommandFailure::usage(
                    "roost api ui close-tab: name the tab with --tab <tab-id>; a close with no \
                     target closes one in every paired tab",
                ));
            }
            Command::CloseTab(Box::new(UiCloseTab {
                session_id: session(1)?,
                ..Default::default()
            }))
        }
        "spotlight" => Command::Spotlight(Box::new(UiSpotlight {
            session_id: session(1)?,
            off: args.has("--off"),
            ..Default::default()
        })),
        other => {
            return Err(CommandFailure::usage(format!(
                "roost api ui: {other:?} is not a ui command. One of navigate, place-split, \
                 select-tab, focus-pane, move-tab, arrange, close-tab, spotlight"
            )));
        }
    };
    let delivered = api
        .answer(
            api.stub().ui_dispatch(UiDispatchRequest {
                target_tab_id: target,
                command: UiCommand {
                    command: Some(command),
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            }),
        )
        .await?
        .delivered;
    out.answer(&delivered.to_string());
    Ok(ExitCode::SUCCESS)
}
