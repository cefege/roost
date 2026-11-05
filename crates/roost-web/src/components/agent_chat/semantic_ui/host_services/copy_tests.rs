//! Browser clipboard completion can land outside Dioxus's current runtime.
//! The real completion handler must retain its owner, preserve failed drafts,
//! and settle only once rather than panic while constructing the reply.

use super::*;
use crate::platform::connect::CoordRpc;
use roost_client_core::ClientCore;
use roost_client_core::client::agent_chat::OmpHostServiceKind;

struct CompletionProbe {
    complete: EventHandler<bool>,
    pump: Pump,
    sending: Signal<BTreeSet<String>>,
    failures: Signal<BTreeMap<String, String>>,
}

fn probe(slot: Rc<RefCell<Option<CompletionProbe>>>) -> Element {
    let pump = use_hook(|| {
        Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("copy-completion"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        )
    });
    let sending = use_signal(|| BTreeSet::from(["copy-1".to_owned()]));
    let failures = use_signal(BTreeMap::<String, String>::new);
    use_hook(|| {
        let service = OmpHostService {
            service_id: "copy-1".into(),
            origin_request_id: "request".into(),
            conversation_id: "conversation".into(),
            view_id: "view".into(),
            transport_id: "transport".into(),
            kind: OmpHostServiceKind::CopyText,
            value: "private clipboard draft".into(),
            cancelled: false,
        };
        pump.write_store(|store| {
            store.omp_host.apply_service(service.clone());
        });
        *slot.borrow_mut() = Some(CompletionProbe {
            complete: copy_completion(pump.clone(), service, sending, failures),
            pump: pump.clone(),
            sending,
            failures,
        });
    });
    rsx! {}
}

#[test]
fn accepted_clipboard_result_outside_runtime_keeps_reply_failure_recoverable() {
    let slot = Rc::new(RefCell::new(None));
    let mut dom = VirtualDom::new_with_props(probe, slot.clone());
    dom.rebuild_in_place();
    let Some(probe) = slot.borrow_mut().take() else {
        panic!("the clipboard probe must mount");
    };
    // Models a resolved browser Promise, with no Dioxus runtime on the stack.
    probe.complete.call(true);
    assert!(!probe.sending.peek().contains("copy-1"));
    assert!(
        probe
            .failures
            .peek()
            .get("copy-1")
            .is_some_and(|error| error.contains("copied") && error.contains("did not confirm"))
    );
    assert_eq!(
        probe
            .pump
            .core()
            .borrow()
            .store()
            .omp_host
            .pending_service_count(),
        1
    );
    let failure = probe.failures.peek().get("copy-1").cloned();
    probe.complete.call(false);
    assert_eq!(probe.failures.peek().get("copy-1").cloned(), failure);
}
