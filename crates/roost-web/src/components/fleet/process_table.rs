//! The process table on a Fleet machine card: sortable headers over the worker's
//! capped process list. Sort state is local to the table, so re-sorting never
//! touches the store.

use dioxus::prelude::*;
use roost_protocol::wire::live_metrics::LiveProcess;

use super::readings::{ProcessSort, ProcessSortColumn, SortDirection, next_sort, sort_processes};
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Icon, IconType, Table, TableBody, TableCell, TableHead,
    TableHeader, TableRow, TableWrapper,
};
use crate::display_format::format_bytes;

#[component]
pub fn ProcessTable(processes: Vec<LiveProcess>) -> Element {
    let sort = use_signal(ProcessSort::default);
    let mut rows = processes;
    sort_processes(&mut rows, sort());
    rsx! {
        TableWrapper {
            Table {
                TableHeader {
                    TableRow {
                        SortHeader { column: ProcessSortColumn::Pid, label: "PID", sort }
                        SortHeader { column: ProcessSortColumn::Name, label: "Name", sort }
                        SortHeader { column: ProcessSortColumn::Cpu, label: "CPU", sort }
                        SortHeader { column: ProcessSortColumn::Memory, label: "Memory", sort }
                    }
                }
                TableBody {
                    if rows.is_empty() {
                        TableRow {
                            TableCell { class: "text-muted-foreground", "No processes reported." }
                        }
                    }
                    for process in rows {
                        TableRow { key: "{process.pid}",
                            TableCell { class: "tabular-nums", "{process.pid}" }
                            TableCell {
                                span { class: "block max-w-xs truncate", "{process.name}" }
                            }
                            TableCell { class: "tabular-nums", "{process.cpu_pct:.1}%" }
                            TableCell { class: "tabular-nums", {format_bytes(process.rss_bytes as f64)} }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SortHeader(
    column: ProcessSortColumn,
    label: &'static str,
    sort: Signal<ProcessSort>,
) -> Element {
    let current = sort();
    let indicator = if current.column != column {
        IconType::ArrowUpDown
    } else if current.direction == SortDirection::Ascending {
        IconType::ArrowUp
    } else {
        IconType::ArrowDown
    };
    rsx! {
        TableHead {
            Button {
                variant: ButtonVariant::Ghost,
                size: ButtonSize::Sm,
                onclick: move |_| sort.set(next_sort(sort(), column)),
                "{label}"
                Icon { icon: indicator, class: "text-muted-foreground" }
            }
        }
    }
}
