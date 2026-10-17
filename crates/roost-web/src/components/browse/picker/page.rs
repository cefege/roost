//! The picker's page body: the focus the sheet must not take, and the
//! composition of the four bands from the store read the shell already holds.
//! Called by `browse::picker::BrowsePicker`; the handlers come from
//! `picker::controls`, the markup from `picker::regions`. It holds no state of
//! its own. Ports `apps/web/src/components/browse/WorkerBrowsePage.tsx`.

use dioxus::prelude::*;

use crate::components::browse::picker::NewFolderForm;
use crate::components::browse::picker::control_set::focus_region;
use crate::components::browse::picker::controls::build_controls;
use crate::components::browse::picker::regions;
use crate::components::browse::view::PickerReading;
use crate::pump::Pump;

/// Whether the machine is absent from a registry that has already published.
fn controls_unavailable(reading: &PickerReading) -> bool {
    !reading.view.scoped && reading.view.hydrated
}

/// One machine's directory, its history and the terminal-launch flow.
#[allow(clippy::too_many_arguments)]
#[component]
pub fn PickerPage(
    pump: Pump,
    worker_fp: String,
    reading: PickerReading,
    compact: bool,
    cursor: Signal<i64>,
    new_folder: Signal<NewFolderForm>,
    ticket: Signal<u64>,
    retry: Signal<u32>,
    hide_middle: Signal<usize>,
    server_menu_open: Signal<bool>,
    crumb_menu_open: Signal<bool>,
    navigate: EventHandler<String>,
) -> Element {
    // The sheet's auto-focus would put the cursor on its own close control,
    // which is a keyboard reader's first Tab away from the entries.
    let unavailable = controls_unavailable(&reading);
    use_effect(use_reactive((&unavailable,), move |(unavailable,)| {
        focus_region(unavailable);
    }));
    // The controls are built before the markup so the handlers can borrow the
    // store read this render already holds.
    let controls = build_controls(
        &pump,
        &worker_fp,
        &reading,
        compact,
        cursor,
        new_folder,
        ticket,
        retry,
        hide_middle,
        server_menu_open,
        crumb_menu_open,
        navigate,
    );

    rsx! {
        regions::PickerRegions {
            pump,
            worker_fp,
            reading,
            compact,
            cursor,
            new_folder,
            server_menu_open,
            crumb_menu_open,
            controls,
        }
    }
}
