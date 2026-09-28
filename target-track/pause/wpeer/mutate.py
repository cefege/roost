import pathlib
import sys

root = pathlib.Path(sys.argv[1]) / "crates/roost-worker/src"
batch = sys.argv[2]


def sub(tag, rel, old, new):
    path = root / rel
    text = path.read_text()
    assert text.count(old) >= 1, (tag, rel, old)
    path.write_text(text.replace(old, new, 1))
    print(tag, rel, "|", old.strip().splitlines()[0][:80])


if batch == "A":
    sub("M1", "peer/packet_lanes.rs",
        "                fragment.commit();\n                core.backpressured[index] = !sent_now;",
        "                if sent_now {\n                    fragment.commit();\n                }\n                core.backpressured[index] = !sent_now;")
    sub("M2", "peer/packet_budget.rs",
        "self.application_bytes + bytes > TERMINAL_PEER_WORKER_APPLICATION_QUEUE_MAX_BYTES", "false")
    sub("M4", "peer/history_reservation.rs",
        "self.base.clone().release(reserved);", "let _ = reserved;")
    sub("M5", "peer/owner_offer.rs",
        "if state.pending.len() >= TERMINAL_PEER_MAX_NEGOTIATIONS_PER_WORKER\n", "if false\n")
    sub("M8", "runtime/downstream/direct.rs",
        "uplink.send_fenced(&reply_fence, frame);",
        "let _ = &reply_fence;\n                uplink.send(frame);")
    sub("M9", "peer/native/driver.rs",
        "let Some(socket) = socket else {", "let Some(socket) = socket.filter(|_| false) else {")
    sub("M10", "runtime/capabilities.rs",
        "    if direct.terminal {", "    if direct.terminal || !direct.terminal {")
    sub("M11", "peer/direct.rs",
        "(request.worker_epoch == self.process_epoch).then(", "(!request.worker_epoch.is_empty()).then(")
    sub("M12", "peer/direct.rs",
        "request.worker_epoch != self.process_epoch || !RETIRE_REASONS.contains(&request.reason.as_str())",
        "request.worker_epoch != self.process_epoch || RETIRE_REASONS.is_empty()")
    sub("M14", "peer/config.rs",
        "if min < 1024 || max > 65_535 || min > max {", "if max > 65_535 || min > max {")
    sub("M15", "peer/native/stun.rs",
        "        port ^= u16::from_be_bytes([cookie[0], cookie[1]]);\n", "")
    sub("M16", "peer/request_validation.rs",
        "        && matches!(third.as_bytes()[0], b'1'..=b'5')\n", "")
else:
    sub("M4", "peer/history_reservation.rs",
        "self.base.clone().release(reserved);", "let _ = reserved;")
    sub("M3", "peer/packet_budget.rs",
        "for (id, handler) in handlers {", "for (id, handler) in handlers.into_iter().take(0) {")
    sub("M6", "peer/owner_offer.rs",
        'format!("{}-smoke-mismatch", request.worker_epoch)', "request.worker_epoch.clone()")
    sub("M7", "peer/packet_lanes.rs",
        "if channel != TerminalPeerPacketLane::Control as usize || !binary {", "if !binary {")
    sub("M17", "peer/coordinator_generation.rs",
        "Some(adopted) => adopted == generation,", "Some(_) => true,")
