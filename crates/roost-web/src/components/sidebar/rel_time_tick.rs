//! The one 30-second ticker every sidebar row's relative age reads, so labels
//! age while the store is silent, paused while the page is hidden. Ports
//! `apps/web/src/components/sidebar/SessionRow.constants.ts` (`relTimeTickMs`;
//! its `ROW_BASE` padding is inlined where the full-density row used it).
//! `SidebarRoot` provides it; `FolderRow` and `SessionRow` read it.

use dioxus::prelude::*;

use crate::pump::use_pump;

/// How often the relative ages refresh.
pub const REL_TIME_TICK_MS: u32 = 30_000;

/// The ticker in context.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RelTimeTicker {
    now_ms: Signal<i64>,
}

/// Provide the ticker below the calling component and start it.
pub fn provide_rel_time_ticker() {
    let pump = use_pump();
    let ticker = use_context_provider(|| RelTimeTicker {
        now_ms: Signal::new(clock_now_ms(&pump)),
    });
    #[cfg(target_arch = "wasm32")]
    use_future(move || {
        let pump = pump.clone();
        async move {
            let mut now_ms = ticker.now_ms;
            loop {
                super::dom::sleep_ms(REL_TIME_TICK_MS).await;
                if super::dom::page_visible() {
                    now_ms.set(clock_now_ms(&pump));
                }
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = ticker;
}

/// The ticker's current reading; subscribes the caller to its ticks.
pub fn use_rel_time_now() -> i64 {
    (use_context::<RelTimeTicker>().now_ms)()
}

fn clock_now_ms(pump: &crate::pump::Pump) -> i64 {
    i64::try_from(pump.core().borrow().clock().now_ms()).unwrap_or(i64::MAX)
}
